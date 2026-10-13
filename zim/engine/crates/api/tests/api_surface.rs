//! Socket-level integration tests for the M5 API surface.
//!
//! Each test boots a private engine (in-memory SQLite + generated
//! loopback certificate + ephemeral HTTPS port) and exercises the
//! spec's acceptance criteria end-to-end with a real TLS client:
//! host validation, version negotiation, the login/cookie flow,
//! structured errors, pagination, ETag/If-Match concurrency,
//! idempotency replay, rate limits, and the 501 family.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use serde_json::{json, Value};
use zim_engine_api::tls::{generate_loopback_certificate, serve_https, TlsMaterial};
use zim_engine_api::{ApiConfig, AppState};
use zim_engine_application::ports::Persistence;
use zim_engine_auth::{ApiKeyManager, SessionManager};
use zim_engine_domain::User;

/// A running test engine plus its client.
struct TestEngine {
    base: String,
    client: reqwest::Client,
    _state: Arc<AppState>,
    persistence: Arc<dyn Persistence>,
}

impl TestEngine {
    /// Boot an engine with the default (spec) configuration.
    async fn start() -> Self {
        Self::start_with(ApiConfig::default()).await
    }

    /// Boot an engine with a custom configuration (rate-limit tests).
    async fn start_with(config: ApiConfig) -> Self {
        let persistence: Arc<dyn Persistence> =
            Arc::new(zim_engine_persistence_sqlite::SqlitePersistence::open_in_memory().unwrap());
        persistence.migrate().unwrap();
        Self::start_on(config, Arc::clone(&persistence)).await
    }

    async fn start_on(config: ApiConfig, persistence: Arc<dyn Persistence>) -> Self {
        let state = Arc::new(AppState::new(
            Arc::clone(&persistence),
            Arc::new(SessionManager::new(Default::default())),
            Arc::new(ApiKeyManager::new()),
            config,
        ));
        let router = zim_engine_api::router(Arc::clone(&state));
        let tls: TlsMaterial = generate_loopback_certificate().unwrap();
        let server = serve_https(router, IpAddr::V4(Ipv4Addr::LOCALHOST), 0, &tls)
            .await
            .unwrap();
        let port = server.local_addr.port();
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap();
        TestEngine {
            base: format!("https://127.0.0.1:{port}/api/v1"),
            client,
            _state: state,
            persistence,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Seed a user directly in persistence (Argon2id-hashed).
    fn seed_user(&self, email: &str, password: &str, admin: bool) -> User {
        let policy = zim_engine_auth::PasswordPolicy::default();
        let hash = zim_engine_auth::password::hash_password(&policy, password).unwrap();
        let user = User {
            id: zim_engine_domain::UserId::new(),
            email: email.to_owned(),
            username: email.split('@').next().unwrap().to_owned(),
            external_id: None,
            password_hash: Some(hash),
            oauth_subject: None,
            is_admin: admin,
            root_admin: admin,
        };
        self.persistence.put_user(&user).unwrap();
        user
    }
}

/// Extract the `Set-Cookie` session value from a response.
fn session_cookie(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .find_map(|v| {
            let raw = v.to_str().ok()?;
            let token = raw.strip_prefix("zim_session=")?.split(';').next()?;
            Some(token.to_owned())
        })
}

/// Login helper returning the session cookie.
async fn login(engine: &TestEngine, email: &str, password: &str) -> Option<String> {
    let response = engine
        .client
        .post(engine.url("/auth/login"))
        .json(&json!({ "email": email, "password": password }))
        .send()
        .await
        .unwrap();
    session_cookie(&response)
}

// ── Transport, headers, versioning ────────────────────────────────

#[tokio::test]
async fn health_advertises_version_and_request_id() {
    let engine = TestEngine::start().await;
    let response = engine
        .client
        .get(engine.url("/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["x-zim-api-version"],
        ApiConfig::default().api_version
    );
    assert!(response
        .headers()
        .get("x-zim-request-id")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("req_"));
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn host_validation_blocks_non_loopback_hosts() {
    let engine = TestEngine::start().await;
    // Attack shape: an attacker-controlled DNS name that resolves to
    // loopback (DNS rebinding) must be rejected before routing.
    let response = engine
        .client
        .get(engine.url("/health"))
        .header("Host", "evil.example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "bad_request");
    assert!(body["error"]["request_id"]
        .as_str()
        .unwrap()
        .starts_with("req_"));

    // localhost is fine.
    let response = engine
        .client
        .get(engine.url("/health"))
        .header("Host", "localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn unsupported_accept_version_is_rejected_with_406() {
    let engine = TestEngine::start().await;
    let response = engine
        .client
        .get(engine.url("/health"))
        .header("Accept", "application/json; version=v2")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 406);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "bad_request");

    // v1 (any minor) passes.
    let response = engine
        .client
        .get(engine.url("/health"))
        .header("Accept", "application/json; version=v1")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
}

// ── Authentication ────────────────────────────────────────────────

#[tokio::test]
async fn login_sets_spec_cookie_attributes_and_refresh_rotates() {
    let engine = TestEngine::start().await;
    engine.seed_user("admin@zim.local", "correct horse", true);

    // Wrong password: uniform 401.
    let response = engine
        .client
        .post(engine.url("/auth/login"))
        .json(&json!({ "email": "admin@zim.local", "password": "wrong" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "unauthorized");

    // Right password: session cookie with the §3 attributes.
    let response = engine
        .client
        .post(engine.url("/auth/login"))
        .json(&json!({ "email": "admin@zim.local", "password": "correct horse" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let cookie = session_cookie(&response).expect("session cookie");
    let set_cookie = response
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("Secure"));
    assert!(set_cookie.contains("SameSite=Strict"));
    assert!(set_cookie.contains("Path=/api/v1"));

    // /users/me works with the cookie; the body never leaks the hash.
    let response = engine
        .client
        .get(engine.url("/users/me"))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["email"], "admin@zim.local");
    assert!(body.get("password_hash").is_none());

    // Refresh rotates the token: the old cookie stops working.
    let response = engine
        .client
        .post(engine.url("/auth/refresh"))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let new_cookie = session_cookie(&response).expect("rotated cookie");
    assert_ne!(cookie, new_cookie);

    let response = engine
        .client
        .get(engine.url("/users/me"))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    let response = engine
        .client
        .get(engine.url("/users/me"))
        .header("Cookie", format!("zim_session={new_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    // Logout kills the successor.
    let response = engine
        .client
        .post(engine.url("/auth/logout"))
        .header("Cookie", format!("zim_session={new_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let response = engine
        .client
        .get(engine.url("/users/me"))
        .header("Cookie", format!("zim_session={new_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn unauthenticated_requests_get_structured_401() {
    let engine = TestEngine::start().await;
    let response = engine
        .client
        .get(engine.url("/users"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "unauthorized");
    assert!(body["error"]["request_id"].is_string());
}

#[tokio::test]
async fn login_brute_force_protection_returns_429() {
    let engine = TestEngine::start().await;
    engine.seed_user("admin@zim.local", "correct horse", true);

    // The per-IP budget is 10 attempts / minute; the 11th is a 429
    // with Retry-After (§9).
    let mut saw_429 = None;
    for i in 0..12 {
        let response = engine
            .client
            .post(engine.url("/auth/login"))
            .json(&json!({ "email": "admin@zim.local", "password": "wrong" }))
            .send()
            .await
            .unwrap();
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            saw_429 = Some(response);
            break;
        }
        assert_eq!(response.status(), 401, "attempt {i} should be a plain 401");
    }
    let response = saw_429.expect("a 429 within 12 attempts");
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .expect("Retry-After header")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(retry_after.parse::<u64>().unwrap() >= 1);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "rate_limited");
    assert_eq!(body["error"]["limit"], 10);
}

// ── Users + API keys ──────────────────────────────────────────────

#[tokio::test]
async fn user_admin_gates_and_crud() {
    let engine = TestEngine::start().await;
    let admin = engine.seed_user("admin@zim.local", "admin pass", true);
    engine.seed_user("member@zim.local", "member pass", false);
    let member_cookie = login(&engine, "member@zim.local", "member pass")
        .await
        .expect("member login");
    let admin_cookie = login(&engine, "admin@zim.local", "admin pass")
        .await
        .expect("admin login");

    // Members cannot list users: structured permission_denied.
    let response = engine
        .client
        .get(engine.url("/users"))
        .header("Cookie", format!("zim_session={member_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "permission_denied");

    // Admin creates a user; the password is Argon2id-verified by a
    // subsequent login.
    let response = engine
        .client
        .post(engine.url("/users"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .json(&json!({
            "email": "Newbie@zim.local",
            "username": "newbie",
            "password": "a good password",
        }))
        .send()
        .await
        .unwrap();
    if response.status() != 201 {
        let status = response.status();
        let body: Value = response.json().await.unwrap();
        panic!("user create failed: {status} {body}");
    }
    let created: Value = response.json().await.unwrap();
    assert!(created.get("password_hash").is_none());

    // Duplicate email → 409.
    let response = engine
        .client
        .post(engine.url("/users"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .json(&json!({
            "email": "newbie@zim.local",
            "username": "newbie2",
            "password": "another password",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "conflict");

    // The new user can log in (email lowercased).
    assert!(login(&engine, "newbie@zim.local", "a good password")
        .await
        .is_some());

    // Self-read via id.
    let response = engine
        .client
        .get(engine.url(&format!("/users/{}", created["id"].as_str().unwrap())))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    // Patch (self-service).
    let response = engine
        .client
        .patch(engine.url(&format!("/users/{}", created["id"].as_str().unwrap())))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .json(&json!({ "username": "newbie-renamed" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["username"], "newbie-renamed");

    // Root admin is undeletable.
    let response = engine
        .client
        .delete(engine.url(&format!("/users/{}", admin.id)))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);

    // Delete the member — nothing depends on them.
    let member = engine
        .persistence
        .get_user_by_email("member@zim.local")
        .unwrap()
        .unwrap();
    let response = engine
        .client
        .delete(engine.url(&format!("/users/{}", member.id)))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    let response = engine
        .client
        .get(engine.url(&format!("/users/{}", member.id)))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "not_found");
}

#[tokio::test]
async fn api_key_bearer_flow() {
    let engine = TestEngine::start().await;
    engine.seed_user("member@zim.local", "member pass", false);
    let member = engine
        .persistence
        .get_user_by_email("member@zim.local")
        .unwrap()
        .unwrap();
    let cookie = login(&engine, "member@zim.local", "member pass")
        .await
        .expect("login");

    // The member holds file.read on a real instance (created by an
    // admin via the API so all FKs hold), so a key scoped to
    // file.read is within their own grants.
    engine.seed_user("admin@zim.local", "admin pass", true);
    let admin_cookie = login(&engine, "admin@zim.local", "admin pass")
        .await
        .expect("admin login");
    let (node, _allocations) = seed_node_with_allocations(&engine, &admin_cookie).await;
    let response = engine
        .client
        .post(engine.url("/instances"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .json(&json!({
            "name": "key-holder",
            "node_id": node["id"].as_str().unwrap(),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let instance: Value = response.json().await.unwrap();
    let instance_id: zim_engine_domain::InstanceId =
        instance["id"].as_str().unwrap().parse().unwrap();
    {
        let mut grants = zim_engine_domain::PermissionSet::new();
        grants.grant(zim_engine_domain::PermissionGrant::FileRead);
        engine
            .persistence
            .put_permission(&zim_engine_domain::Permission {
                user_id: member.id,
                instance_id,
                grants,
            })
            .unwrap();
    }

    // Issue a key (self-service).
    let response = engine
        .client
        .post(engine.url(&format!("/users/{}/api-keys", member.id)))
        .header("Cookie", format!("zim_session={cookie}"))
        .json(&json!({ "scopes": ["file.read"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let body: Value = response.json().await.unwrap();
    let full_key = body["data"]["key"].as_str().unwrap().to_owned();
    assert!(full_key.starts_with("zim_"));
    assert_eq!(body["data"]["scopes"], json!(["file.read"]));

    // The full key is shown exactly once — the listing carries no
    // secret and no hash.
    let response = engine
        .client
        .get(engine.url(&format!("/users/{}/api-keys", member.id)))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let listed: Value = response.json().await.unwrap();
    assert_eq!(listed["data"].as_array().unwrap().len(), 1);
    assert!(listed["data"][0].get("key").is_none());
    assert!(listed["data"][0].get("secret_hash").is_none());

    // Bearer works for /users/me.
    let response = engine
        .client
        .get(engine.url("/users/me"))
        .header("Authorization", format!("Bearer {full_key}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    // Unknown scopes are rejected up front.
    let response = engine
        .client
        .post(engine.url(&format!("/users/{}/api-keys", member.id)))
        .header("Cookie", format!("zim_session={cookie}"))
        .json(&json!({ "scopes": ["not.a.grant"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "validation_failed");

    // Revocation: the key dies.
    let key_id = listed["data"][0]["key_id"].as_str().unwrap().to_owned();
    let response = engine
        .client
        .delete(engine.url(&format!("/users/{}/api-keys/{}", member.id, key_id)))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    let response = engine
        .client
        .get(engine.url("/users/me"))
        .header("Authorization", format!("Bearer {full_key}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}

// ── Instances: ETags, If-Match, visibility, workloads alias ───────

/// Seed node + two free allocations; returns (node_id, allocations).
async fn seed_node_with_allocations(engine: &TestEngine, cookie: &str) -> (Value, Vec<Value>) {
    let response = engine
        .client
        .post(engine.url("/nodes"))
        .header("Cookie", format!("zim_session={cookie}"))
        .json(&json!({ "name": "local-1", "fqdn": "localhost" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let node: Value = response.json().await.unwrap();

    let response = engine
        .client
        .post(engine.url(&format!(
            "/nodes/{}/allocations",
            node["id"].as_str().unwrap()
        )))
        .header("Cookie", format!("zim_session={cookie}"))
        .json(&json!({ "port": 25565 }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let allocation_a: Value = response.json().await.unwrap();

    let response = engine
        .client
        .post(engine.url(&format!(
            "/nodes/{}/allocations",
            node["id"].as_str().unwrap()
        )))
        .header("Cookie", format!("zim_session={cookie}"))
        .json(&json!({ "port": 25566 }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let allocation_b: Value = response.json().await.unwrap();

    (node, vec![allocation_a, allocation_b])
}

#[tokio::test]
async fn instance_concurrency_control_via_etag() {
    let engine = TestEngine::start().await;
    let admin = engine.seed_user("admin@zim.local", "admin pass", true);
    engine.seed_user("stranger@zim.local", "stranger pass", false);
    let admin_cookie = login(&engine, "admin@zim.local", "admin pass")
        .await
        .expect("admin login");
    let stranger_cookie = login(&engine, "stranger@zim.local", "stranger pass")
        .await
        .expect("stranger login");

    let (node, _allocations) = seed_node_with_allocations(&engine, &admin_cookie).await;

    // Create an instance (admin; auto-picks the free allocation).
    let response = engine
        .client
        .post(engine.url("/instances"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .json(&json!({
            "name": "test-server",
            "node_id": node["id"].as_str().unwrap(),
            "owner_id": admin.id.to_string(),
        }))
        .send()
        .await
        .unwrap();
    if response.status() != 201 {
        let status = response.status();
        let body: Value = response.json().await.unwrap();
        panic!("instance create failed: {status} {body}");
    }
    let instance: Value = response.json().await.unwrap();
    let instance_id = instance["id"].as_str().unwrap().to_owned();
    let path = format!("/instances/{instance_id}");

    // Reads carry the ETag.
    let response = engine
        .client
        .get(engine.url(&path))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .expect("ETag header")
        .to_str()
        .unwrap()
        .to_owned();

    // Strangers see nothing (uniform 404).
    let response = engine
        .client
        .get(engine.url(&path))
        .header("Cookie", format!("zim_session={stranger_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);

    // PATCH without If-Match → 400 (fail closed).
    let response = engine
        .client
        .patch(engine.url(&path))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .json(&json!({ "name": "renamed" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);

    // Stale If-Match → 412 with the current ETag in the body.
    let response = engine
        .client
        .patch(engine.url(&path))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .header("If-Match", "\"inst-stale\"")
        .json(&json!({ "name": "renamed" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 412);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "precondition_failed");
    assert_eq!(body["error"]["current_etag"], serde_json::json!(etag));

    // Current If-Match → 200, new ETag.
    let response = engine
        .client
        .patch(engine.url(&path))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .header("If-Match", &etag)
        .json(&json!({ "name": "renamed", "backup_limit": 5 }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let new_etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .expect("new ETag")
        .to_str()
        .unwrap()
        .to_owned();
    assert_ne!(etag, new_etag);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["name"], "renamed");
    assert_eq!(body["backup_limit"], 5);

    // DELETE requires the fresh If-Match.
    let response = engine
        .client
        .delete(engine.url(&path))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .header("If-Match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 412);
    let response = engine
        .client
        .delete(engine.url(&path))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .header("If-Match", &new_etag)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);

    // Gone.
    let response = engine
        .client
        .get(engine.url(&path))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn workloads_surface_aliases_instances() {
    let engine = TestEngine::start().await;
    let admin = engine.seed_user("admin@zim.local", "admin pass", true);
    let admin_cookie = login(&engine, "admin@zim.local", "admin pass")
        .await
        .expect("admin login");
    let (node, _allocations) = seed_node_with_allocations(&engine, &admin_cookie).await;

    let response = engine
        .client
        .post(engine.url("/workloads"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .json(&json!({
            "name": "aliased",
            "node_id": node["id"].as_str().unwrap(),
            "owner_id": admin.id.to_string(),
            "kind": "game_server",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let workload: Value = response.json().await.unwrap();
    let id = workload["id"].as_str().unwrap().to_owned();

    // The alias returns the same representation (§10.17).
    let via_instances: Value = engine
        .client
        .get(engine.url(&format!("/instances/{id}")))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let via_workloads: Value = engine
        .client
        .get(engine.url(&format!("/workloads/{id}")))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(via_instances, via_workloads);

    // Unknown kinds are 501 (the typed workload abstraction is M16).
    let response = engine
        .client
        .post(engine.url("/workloads"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .json(&json!({
            "name": "a database",
            "node_id": node["id"].as_str().unwrap(),
            "kind": "database",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 501);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "not_implemented");
}

#[tokio::test]
async fn instance_state_and_lifecycle_stub() {
    let engine = TestEngine::start().await;
    let admin = engine.seed_user("admin@zim.local", "admin pass", true);
    let admin_cookie = login(&engine, "admin@zim.local", "admin pass")
        .await
        .expect("admin login");
    let (node, _allocations) = seed_node_with_allocations(&engine, &admin_cookie).await;

    let response = engine
        .client
        .post(engine.url("/instances"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .json(&json!({
            "name": "stubby",
            "node_id": node["id"].as_str().unwrap(),
            "owner_id": admin.id.to_string(),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let instance: Value = response.json().await.unwrap();
    let id = instance["id"].as_str().unwrap().to_owned();

    // The small state view.
    let response = engine
        .client
        .get(engine.url(&format!("/instances/{id}/state")))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["status"], "offline");

    // Lifecycle is declared but not implemented (M6).
    let response = engine
        .client
        .post(engine.url(&format!("/instances/{id}/start")))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 501);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "not_implemented");
    assert_eq!(
        body["error"]["milestone"],
        "M6 (runtime abstraction + Docker runtime)"
    );

    // Eggs are M9.
    let response = engine
        .client
        .get(engine.url("/eggs"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 501);
}

// ── Pagination ────────────────────────────────────────────────────

#[tokio::test]
async fn page_pagination_envelope() {
    let engine = TestEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let admin_cookie = login(&engine, "admin@zim.local", "admin pass")
        .await
        .expect("admin login");

    for i in 0..2 {
        let response = engine
            .client
            .post(engine.url("/users"))
            .header("Cookie", format!("zim_session={admin_cookie}"))
            .json(&json!({
                "email": format!("user{i}@zim.local"),
                "username": format!("user{i}"),
                "password": "some password",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 201);
    }

    // 3 users total (admin + 2); page 2 of per_page 2 → 1 item.
    let response = engine
        .client
        .get(engine.url("/users?page=2&per_page=2"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["data"].as_array().unwrap().len(), 1);
    assert_eq!(body["pagination"]["page"], 2);
    assert_eq!(body["pagination"]["per_page"], 2);
    assert_eq!(body["pagination"]["total"], 3);
    assert_eq!(body["pagination"]["total_pages"], 2);

    // Bad page values are surfaced, not coerced.
    let response = engine
        .client
        .get(engine.url("/users?page=zero"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "validation_failed");
}

// ── Idempotency ───────────────────────────────────────────────────

#[tokio::test]
async fn idempotency_key_replays_and_scopes() {
    let engine = TestEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let admin_cookie = login(&engine, "admin@zim.local", "admin pass")
        .await
        .expect("admin login");

    // First request with the key.
    let response = engine
        .client
        .post(engine.url("/users"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .header("Idempotency-Key", "create-nb-1")
        .json(&json!({
            "email": "idem@zim.local",
            "username": "idem",
            "password": "a password",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let first: Value = response.json().await.unwrap();

    // A duplicate send replays the stored response verbatim.
    let response = engine
        .client
        .post(engine.url("/users"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .header("Idempotency-Key", "create-nb-1")
        .json(&json!({
            "email": "idem@zim.local",
            "username": "idem",
            "password": "a password",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    assert_eq!(response.headers()["x-zim-idempotent-replay"], "true");
    let replayed: Value = response.json().await.unwrap();
    assert_eq!(first["id"], replayed["id"]);

    // Only one user was created.
    let listed: Value = engine
        .client
        .get(engine.url("/users"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["pagination"]["total"], 2);

    // Reusing the key for a *different* request is an error.
    let response = engine
        .client
        .post(engine.url("/users"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .header("Idempotency-Key", "create-nb-1")
        .json(&json!({
            "email": "other@zim.local",
            "username": "other",
            "password": "a password",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "bad_request");
}

// ── Subusers and grants ───────────────────────────────────────────

#[tokio::test]
async fn subuser_grants_gate_instance_surface() {
    let engine = TestEngine::start().await;
    let admin = engine.seed_user("admin@zim.local", "admin pass", true);
    let member = engine.seed_user("member@zim.local", "member pass", false);
    let admin_cookie = login(&engine, "admin@zim.local", "admin pass")
        .await
        .expect("admin login");
    let member_cookie = login(&engine, "member@zim.local", "member pass")
        .await
        .expect("member login");

    let (node, _allocations) = seed_node_with_allocations(&engine, &admin_cookie).await;
    let response = engine
        .client
        .post(engine.url("/instances"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .json(&json!({
            "name": "shared-server",
            "node_id": node["id"].as_str().unwrap(),
            "owner_id": admin.id.to_string(),
        }))
        .send()
        .await
        .unwrap();
    let instance: Value = response.json().await.unwrap();
    let id = instance["id"].as_str().unwrap().to_owned();

    // The owner adds the member as subuser with schedule.read only.
    let response = engine
        .client
        .post(engine.url(&format!("/instances/{id}/subusers")))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .json(&json!({
            "user_id": member.id.to_string(),
            "grants": ["schedule.read"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);

    // The subuser can now see the instance...
    let response = engine
        .client
        .get(engine.url(&format!("/instances/{id}")))
        .header("Cookie", format!("zim_session={member_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    // ...and list schedules (the granted capability)...
    let response = engine
        .client
        .get(engine.url(&format!("/instances/{id}/schedules")))
        .header("Cookie", format!("zim_session={member_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["data"].as_array().map(Vec::len), Some(0));

    // ...but not manage them: 403 with the missing grant listed.
    let response = engine
        .client
        .post(engine.url(&format!("/instances/{id}/schedules")))
        .header("Cookie", format!("zim_session={member_cookie}"))
        .json(&json!({
            "name": "daily restart",
            "cron": "0 4 * * *",
            "actions": [{ "type": "power", "action": "restart" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "permission_denied");
    assert_eq!(
        body["error"]["missing_permissions"],
        json!(["schedule.create"])
    );

    // The listing shows the subuser row.
    let response = engine
        .client
        .get(engine.url(&format!("/instances/{id}/subusers")))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["data"][0]["email"], "member@zim.local");
    assert_eq!(body["data"][0]["grants"], json!(["schedule.read"]));
}

// ── Schedules CRUD ────────────────────────────────────────────────

#[tokio::test]
async fn schedule_crud_and_validation() {
    let engine = TestEngine::start().await;
    let admin = engine.seed_user("admin@zim.local", "admin pass", true);
    let admin_cookie = login(&engine, "admin@zim.local", "admin pass")
        .await
        .expect("admin login");
    let (node, _allocations) = seed_node_with_allocations(&engine, &admin_cookie).await;
    let response = engine
        .client
        .post(engine.url("/instances"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .json(&json!({
            "name": "sched-server",
            "node_id": node["id"].as_str().unwrap(),
            "owner_id": admin.id.to_string(),
        }))
        .send()
        .await
        .unwrap();
    let instance: Value = response.json().await.unwrap();
    let id = instance["id"].as_str().unwrap().to_owned();

    // Malformed cron → validation_failed.
    let response = engine
        .client
        .post(engine.url(&format!("/instances/{id}/schedules")))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .json(&json!({
            "name": "bad",
            "cron": "not cron",
            "actions": [{ "type": "power", "action": "stop" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "validation_failed");

    // Good create.
    let response = engine
        .client
        .post(engine.url(&format!("/instances/{id}/schedules")))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .json(&json!({
            "name": "daily restart",
            "cron": "0 4 * * *",
            "is_active": true,
            "actions": [{ "type": "power", "action": "restart" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let schedule: Value = response.json().await.unwrap();
    let sid = schedule["id"].as_str().unwrap().to_owned();
    assert_eq!(schedule["next_run_at"], Value::Null);

    // Patch: deactivate.
    let response = engine
        .client
        .patch(engine.url(&format!("/instances/{id}/schedules/{sid}")))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .json(&json!({ "is_active": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["is_active"], false);

    // "Run now" is the scheduler's business (M9).
    let response = engine
        .client
        .post(engine.url(&format!("/instances/{id}/schedules/{sid}/run")))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 501);

    // Delete.
    let response = engine
        .client
        .delete(engine.url(&format!("/instances/{id}/schedules/{sid}")))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
}

// ── Audit cursor pagination ───────────────────────────────────────

#[tokio::test]
async fn audit_log_walks_with_opaque_cursors() {
    let engine = TestEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let admin_cookie = login(&engine, "admin@zim.local", "admin pass")
        .await
        .expect("admin login");

    // Produce audit records: 3 user creations + the login.
    for i in 0..3 {
        let response = engine
            .client
            .post(engine.url("/users"))
            .header("Cookie", format!("zim_session={admin_cookie}"))
            .json(&json!({
                "email": format!("u{i}@zim.local"),
                "username": format!("u{i}"),
                "password": "a password",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 201);
    }

    // Members cannot read the audit log.
    let response = engine
        .client
        .get(engine.url("/audit"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);

    // Walk in pages of 2 with the opaque cursor.
    let mut cursor: Option<String> = None;
    let mut records = Vec::new();
    for _ in 0..4 {
        let url = match &cursor {
            None => engine.url("/audit?limit=2"),
            Some(c) => engine.url(&format!("/audit?limit=2&cursor={c}")),
        };
        let response = engine
            .client
            .get(url)
            .header("Cookie", format!("zim_session={admin_cookie}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await.unwrap();
        let page = body["data"].as_array().unwrap().clone();
        let has_more = body["pagination"]["has_more"].as_bool().unwrap();
        records.extend(page);
        let next = body["pagination"]["next_cursor"]
            .as_str()
            .map(str::to_owned);
        if !has_more {
            assert!(next.is_none());
            break;
        }
        cursor = next;
    }
    // login + 3 creations ≥ 4 records, all ids strictly ascending.
    assert!(records.len() >= 4);
    let ids: Vec<u64> = records.iter().map(|r| r["id"].as_u64().unwrap()).collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]));
    assert!(records
        .iter()
        .any(|r| r["action"].as_str().unwrap() == "user.create"));

    // Garbage cursors are 400s.
    let response = engine
        .client
        .get(engine.url("/audit?cursor=%21%21%21"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
}

// ── Rate limits per credential ────────────────────────────────────

#[tokio::test]
async fn per_credential_rate_limit_returns_429_with_headers() {
    let config = ApiConfig {
        rate_limit_per_credential: 5,
        ..ApiConfig::default()
    };
    let engine = TestEngine::start_with(config).await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let cookie = login(&engine, "admin@zim.local", "admin pass")
        .await
        .expect("admin login");

    let mut blocked = None;
    for _ in 0..7 {
        let response = engine
            .client
            .get(engine.url("/users/me"))
            .header("Cookie", format!("zim_session={cookie}"))
            .send()
            .await
            .unwrap();
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            blocked = Some(response);
            break;
        }
        assert_eq!(response.status(), 200);
    }
    let response = blocked.expect("a 429 within 7 requests");
    let headers = response.headers();
    assert!(headers.contains_key("x-zim-ratelimit-limit"));
    assert!(headers.contains_key("x-zim-ratelimit-remaining"));
    assert!(headers.contains_key("x-zim-ratelimit-reset"));
    assert!(headers.contains_key(reqwest::header::RETRY_AFTER));
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "rate_limited");
    assert_eq!(body["error"]["limit"], 5);

    // A different credential is unaffected.
    engine.seed_user("other@zim.local", "other pass", false);
    let other_cookie = login(&engine, "other@zim.local", "other pass")
        .await
        .expect("second login");
    let response = engine
        .client
        .get(engine.url("/users/me"))
        .header("Cookie", format!("zim_session={other_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
}

// ── Extensions / jobs listings ────────────────────────────────────

#[tokio::test]
async fn extensions_and_jobs_listings() {
    let engine = TestEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let admin_cookie = login(&engine, "admin@zim.local", "admin pass")
        .await
        .expect("admin login");

    let response = engine
        .client
        .get(engine.url("/extensions"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["data"].as_array().map(Vec::len), Some(0));

    // Jobs listing is instance-scoped in M5; the filter is required.
    let response = engine
        .client
        .get(engine.url("/jobs"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "validation_failed");
}

// ── Unsupported content type ──────────────────────────────────────

#[tokio::test]
async fn wrong_content_type_is_415() {
    let engine = TestEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let admin_cookie = login(&engine, "admin@zim.local", "admin pass")
        .await
        .expect("admin login");

    let response = engine
        .client
        .post(engine.url("/users"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .header("Content-Type", "text/plain")
        .body("not json")
        .send()
        .await
        .unwrap();
    // axum's Json extractor rejects before the handler runs.
    assert_eq!(response.status(), 415);
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, 415);
    assert!(body.get("error").is_some());
}

#[tokio::test]
async fn allocations_duplicate_and_primary_rules() {
    let engine = TestEngine::start().await;
    let admin = engine.seed_user("admin@zim.local", "admin pass", true);
    let admin_cookie = login(&engine, "admin@zim.local", "admin pass")
        .await
        .expect("admin login");
    let (node, allocations) = seed_node_with_allocations(&engine, &admin_cookie).await;

    // Duplicate (ip, port) on the node → 409.
    let response = engine
        .client
        .post(engine.url(&format!(
            "/nodes/{}/allocations",
            node["id"].as_str().unwrap()
        )))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .json(&json!({ "port": 25565 }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);

    // Create an instance (claims the first allocation)...
    let response = engine
        .client
        .post(engine.url("/instances"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .json(&json!({
            "name": "alloc-server",
            "node_id": node["id"].as_str().unwrap(),
            "owner_id": admin.id.to_string(),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let instance: Value = response.json().await.unwrap();
    let id = instance["id"].as_str().unwrap().to_owned();
    let primary = instance["allocation_id"].as_u64().unwrap();

    // ...the instance's allocation listing shows the claim.
    let response = engine
        .client
        .get(engine.url(&format!("/instances/{id}/allocations")))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["data"]["assigned_to"].as_str().unwrap(), id);

    // Unassigning the primary → 409 (swap first).
    let response = engine
        .client
        .delete(engine.url(&format!("/instances/{id}/allocations/{primary}")))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);

    // Assign the second (free) allocation as the new primary.
    let free = allocations[1]["id"].as_u64().unwrap();
    let response = engine
        .client
        .post(engine.url(&format!("/instances/{id}/allocations")))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .json(&json!({ "allocation_id": free }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    // Now the OLD primary is no longer primary and can be unassigned.
    let response = engine
        .client
        .delete(engine.url(&format!("/instances/{id}/allocations/{primary}")))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
}
