//! Socket-level integration tests for the M31 managed-database
//! family (spec §10.21): the admin host surface (`/hosts`) and the
//! per-instance database surface (`/instances/{id}/databases`).
//!
//! The acceptance walk mirrors the M28/M29 families: the full CRUD
//! round over a real TLS engine, the authz matrix (the
//! `database.*` grants, including `view_password`'s own gate), the
//! limit refusals (structured 409s that name their reason), the
//! credential discipline (passwords only where the contract says),
//! and the cross-instance uniform-404 rule.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use serde_json::{json, Value};
use zim_engine_api::tls::{generate_loopback_certificate, serve_https, TlsMaterial};
use zim_engine_api::{ApiConfig, AppState};
use zim_engine_application::ports::Persistence;
use zim_engine_application::supervision::Supervisor;
use zim_engine_auth::{ApiKeyManager, SessionManager};
use zim_engine_domain::{InstanceId, User};

/// A running test engine plus its client (the api_surface harness
/// shape, trimmed to what this family needs).
struct TestEngine {
    base: String,
    client: reqwest::Client,
    /// Held alive for the server's lifetime (the router borrows it).
    _state: Arc<AppState>,
    persistence: Arc<dyn Persistence>,
}

impl TestEngine {
    async fn start() -> Self {
        let persistence: Arc<dyn Persistence> =
            Arc::new(zim_engine_persistence_sqlite::SqlitePersistence::open_in_memory().unwrap());
        persistence.migrate().unwrap();

        // The local node the supervisor manages (instances created
        // through the API land on it).
        let nodes = persistence.list_nodes().unwrap();
        if nodes.is_empty() {
            let node = zim_engine_domain::Node {
                id: zim_engine_domain::NodeId::new(),
                name: "local-test".to_owned(),
                location_id: zim_engine_domain::LocationId::new(),
                fqdn: "localhost".to_owned(),
                scheme: zim_engine_domain::Scheme::Https,
                public_port: 8443,
                listen_port: 8444,
                capabilities: Default::default(),
                deployment: zim_engine_domain::NodeDeployment::LocalLinux,
                last_heartbeat: None,
                last_seen_state: Default::default(),
            };
            persistence.put_node(&node).unwrap();
        }
        let supervisor = Supervisor::new(
            Arc::clone(&persistence),
            Arc::new(
                zim_engine_application::supervision::testing::MockRuntimeFactory::new(
                    zim_engine_application::supervision::testing::MockRuntime::always_running(),
                ),
            ),
            zim_engine_application::supervision::SupervisionPolicy::default(),
            std::env::temp_dir().join("zim-api-databases-test"),
        );
        supervisor.load_local().await.unwrap();

        let data_root = std::env::temp_dir().join("zim-api-databases-test");
        let jobs = zim_engine_jobs::JobSystem::new(
            Arc::clone(&persistence),
            None,
            zim_engine_jobs::JobPolicy {
                poll_interval: std::time::Duration::from_millis(20),
                cleaner_interval: std::time::Duration::from_millis(50),
                max_concurrent: 2,
                shutdown_grace: shutdown_grace_for_tests(),
            },
        );
        let backups = zim_engine_backups::BackupService::new(
            Arc::clone(&persistence),
            Arc::clone(&jobs),
            data_root.clone(),
        );
        backups.register().await;
        let transfers = zim_engine_transfers::TransferService::new(
            Arc::clone(&persistence),
            Arc::clone(&jobs),
            Arc::clone(&supervisor),
            data_root.clone(),
        );
        transfers.register().await;
        let installs = zim_engine_installs::InstallService::new(
            Arc::clone(&persistence),
            Arc::clone(&jobs),
            Arc::clone(&supervisor),
            Arc::new(
                zim_engine_application::supervision::testing::MockInstallRunner::always_succeeds(),
            ),
            data_root.clone(),
        );
        installs.register().await;
        jobs.start();
        let scheduler =
            zim_engine_scheduler::Scheduler::new(Arc::clone(&persistence), Arc::clone(&supervisor));
        let data_dir =
            std::env::temp_dir().join(format!("zim-api-databases-{}", uuid::Uuid::new_v4()));
        let frontend = Arc::new(zim_engine_frontend_loader::FrontendService::new(
            data_dir.clone(),
            semver::Version::new(0, 0, 1),
            "v1.0.0",
        ));
        let extensions = Arc::new(zim_engine_extensions::ExtensionsService::new(
            Arc::clone(&persistence),
            Arc::clone(&frontend),
            &data_dir,
            semver::Version::new(0, 0, 1),
            "v1.0.0",
        ));
        let themes = Arc::new(zim_engine_extensions::ThemeService::new(&data_dir));
        let state = Arc::new(AppState::new(
            Arc::clone(&persistence),
            supervisor,
            Arc::new(SessionManager::new(Default::default())),
            Arc::new(ApiKeyManager::new()),
            Arc::new(zim_engine_auth::TabTokenManager::new(Default::default())),
            Arc::clone(&jobs),
            Arc::clone(&backups),
            Arc::clone(&transfers),
            Arc::clone(&installs),
            Arc::clone(&scheduler),
            zim_engine_telemetry::TelemetryService::new(
                Arc::clone(&persistence),
                Default::default(),
            ),
            Arc::clone(&frontend),
            Arc::clone(&extensions),
            Arc::clone(&themes),
            zim_engine_api::control::ControlPlane::for_tests(),
            ApiConfig::default(),
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

    async fn login(&self, email: &str, password: &str) -> String {
        let response = self
            .client
            .post(self.url("/auth/login"))
            .json(&json!({ "email": email, "password": password }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "login must succeed for {email}");
        response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .find_map(|v| {
                let raw = v.to_str().ok()?;
                let token = raw.strip_prefix("zim_session=")?.split(';').next()?;
                Some(token.to_owned())
            })
            .expect("login sets the session cookie")
    }

    /// Create an instance through the API with an explicit database
    /// limit (None = feature off is the refusal-under-test's shape).
    /// A fresh allocation is claimed first (the node's free pool
    /// starts empty; each call mints a distinct port).
    async fn create_instance(&self, cookie: &str, database_limit: Option<u32>) -> Value {
        let node_id = self.persistence.list_nodes().unwrap()[0].id.to_string();
        let port = 30000 + (uuid::Uuid::new_v4().as_bytes()[0] as u16 % 20000);
        let response = self
            .client
            .post(self.url(&format!("/nodes/{node_id}/allocations")))
            .header("Cookie", format!("zim_session={cookie}"))
            .json(&json!({ "port": port }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 201, "allocation create must succeed");
        let mut body = json!({ "name": "db-holder", "node_id": node_id });
        if let Some(limit) = database_limit {
            body["database_limit"] = json!(limit);
        }
        let response = self
            .client
            .post(self.url("/instances"))
            .header("Cookie", format!("zim_session={cookie}"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 201, "instance create must succeed");
        response.json().await.unwrap()
    }

    /// Register a host through the admin API.
    async fn create_host(&self, cookie: &str, name: &str) -> Value {
        let response = self
            .client
            .post(self.url("/hosts"))
            .header("Cookie", format!("zim_session={cookie}"))
            .json(&json!({
                "name": name,
                "host": format!("{name}.internal"),
                "port": 3306,
                "username": "zim_admin",
                "password": "host-secret",
                "max_databases": 50,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 201, "host create must succeed");
        response.json().await.unwrap()
    }
}

fn shutdown_grace_for_tests() -> std::time::Duration {
    std::time::Duration::from_secs(5)
}

// ── The admin host family ─────────────────────────────────────────

#[tokio::test]
async fn host_crud_round_trips_without_ever_reading_the_password() {
    let engine = TestEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let cookie = engine.login("admin@zim.local", "admin pass").await;

    // Create: 201, the wire shape, no password field.
    let response = engine
        .client
        .post(engine.url("/hosts"))
        .header("Cookie", format!("zim_session={cookie}"))
        .json(&json!({
            "name": "main-mysql",
            "host": "db.internal",
            "port": 3306,
            "username": "zim_admin",
            "password": "host-secret",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let host: Value = response.json().await.unwrap();
    assert_eq!(host["object"], "database_host");
    assert_eq!(host["name"], "main-mysql");
    assert_eq!(host["host"], "db.internal");
    assert_eq!(host["port"], 3306);
    assert_eq!(host["databases_count"], 0);
    assert!(
        host.get("password").is_none(),
        "the credential is write-only"
    );

    // Detail + list agree; neither leaks the credential.
    let hid = host["id"].as_str().unwrap().to_owned();
    let detail: Value = engine
        .client
        .get(engine.url(&format!("/hosts/{hid}")))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["id"], host["id"]);
    assert!(detail.get("password").is_none());

    let list: Value = engine
        .client
        .get(engine.url("/hosts"))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["data"].as_array().unwrap().len(), 1);

    // PATCH renames (and a collision is a 409, not a 500).
    let response = engine
        .client
        .patch(engine.url(&format!("/hosts/{hid}")))
        .header("Cookie", format!("zim_session={cookie}"))
        .json(&json!({ "name": "renamed-mysql" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let renamed: Value = response.json().await.unwrap();
    assert_eq!(renamed["name"], "renamed-mysql");
    assert!(renamed.get("password").is_none());

    // A duplicate name is refused up front.
    let response = engine
        .client
        .post(engine.url("/hosts"))
        .header("Cookie", format!("zim_session={cookie}"))
        .json(&json!({
            "name": "renamed-mysql",
            "host": "other.internal",
            "port": 3307,
            "username": "other_admin",
            "password": "other-secret",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "conflict");

    // Validation is structured 422 (the Panel's grammars).
    let response = engine
        .client
        .post(engine.url("/hosts"))
        .header("Cookie", format!("zim_session={cookie}"))
        .json(&json!({
            "name": "bad-port",
            "host": "bad.internal",
            "port": 0,
            "username": "zim_admin",
            "password": "secret",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "validation_failed");

    // DELETE: clean (no databases yet), 204.
    let response = engine
        .client
        .delete(engine.url(&format!("/hosts/{hid}")))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);

    // A non-admin is refused on every leg (403).
    let _member = engine.seed_user("member@zim.local", "member pass", false);
    let member_cookie = engine.login("member@zim.local", "member pass").await;
    for method in ["get", "post"] {
        let response = match method {
            "get" => engine
                .client
                .get(engine.url("/hosts"))
                .header("Cookie", format!("zim_session={member_cookie}"))
                .send()
                .await
                .unwrap(),
            _ => engine
                .client
                .post(engine.url("/hosts"))
                .header("Cookie", format!("zim_session={member_cookie}"))
                .json(&json!({ "name": "x", "host": "x.internal", "port": 1, "username": "x", "password": "y" }))
                .send()
                .await
                .unwrap(),
        };
        assert_eq!(
            response.status(),
            403,
            "non-admin {method} /hosts must be 403"
        );
    }
}

// ── The per-instance database family ──────────────────────────────

#[tokio::test]
async fn database_create_lists_rotates_and_deletes_with_credential_discipline() {
    let engine = TestEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let cookie = engine.login("admin@zim.local", "admin pass").await;
    engine.create_host(&cookie, "main-mysql").await;
    let instance = engine.create_instance(&cookie, Some(5)).await;
    let instance_id = instance["id"].as_str().unwrap().to_owned();
    let auth = format!("zim_session={cookie}");

    // Create: 201, the derived name/username, the one-time password.
    let response = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/databases")))
        .header("Cookie", &auth)
        .json(&json!({ "name": "world", "remote": "%" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let database: Value = response.json().await.unwrap();
    let short = instance["uuid_short"].as_str().unwrap();
    assert_eq!(
        database["name"],
        format!("s{short}_world"),
        "the name carries the Panel's s{{short}}_ namespace"
    );
    assert!(
        database["username"]
            .as_str()
            .unwrap()
            .starts_with(&format!("u{short}_")),
        "the username carries the u{{short}}_ form: {}",
        database["username"]
    );
    let password = database["password"].as_str().unwrap().to_owned();
    assert_eq!(password.chars().count(), 24, "the Panel's 24-char shape");
    assert_eq!(database["connections"]["host"], "main-mysql.internal");
    assert_eq!(database["connections"]["port"], 3306);
    let dbid = database["id"].as_str().unwrap().to_owned();

    // List and detail NEVER carry the password.
    for path in [
        format!("/instances/{instance_id}/databases"),
        format!("/instances/{instance_id}/databases/{dbid}"),
    ] {
        let body: Value = engine
            .client
            .get(engine.url(&path))
            .header("Cookie", &auth)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let rows = if path.ends_with("databases") {
            body["data"].as_array().unwrap().clone()
        } else {
            vec![body]
        };
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].get("password").is_none(),
            "list/detail never carry the password"
        );
        assert_eq!(rows[0]["name"], format!("s{short}_world"));
    }

    // The gated password read: the admin (owner-implicit + admin)
    // passes the view_password grant.
    let body: Value = engine
        .client
        .get(engine.url(&format!(
            "/instances/{instance_id}/databases/{dbid}/password"
        )))
        .header("Cookie", &auth)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["password"], password);

    // Rotate: a NEW password, carried once in the response.
    let body: Value = engine
        .client
        .post(engine.url(&format!(
            "/instances/{instance_id}/databases/{dbid}/rotate-password"
        )))
        .header("Cookie", &auth)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rotated = body["password"].as_str().unwrap().to_owned();
    assert_eq!(rotated.chars().count(), 24);
    assert_ne!(rotated, password, "rotation actually rotates");

    // The gated read agrees with the rotation.
    let body: Value = engine
        .client
        .get(engine.url(&format!(
            "/instances/{instance_id}/databases/{dbid}/password"
        )))
        .header("Cookie", &auth)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["password"], rotated);

    // Delete: 204, and the uniform 404 afterwards.
    let response = engine
        .client
        .delete(engine.url(&format!("/instances/{instance_id}/databases/{dbid}")))
        .header("Cookie", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    let response = engine
        .client
        .get(engine.url(&format!("/instances/{instance_id}/databases/{dbid}")))
        .header("Cookie", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn database_create_refusals_are_structured_and_name_their_reason() {
    let engine = TestEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let cookie = engine.login("admin@zim.local", "admin pass").await;
    engine.create_host(&cookie, "capped-mysql").await;
    let instance = engine.create_instance(&cookie, Some(1)).await;
    let instance_id = instance["id"].as_str().unwrap().to_owned();
    let auth = format!("zim_session={cookie}");

    // No hosts at all is a 409 naming the condition — but here one
    // exists, so the first create succeeds.
    let response = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/databases")))
        .header("Cookie", &auth)
        .json(&json!({ "name": "first" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);

    // The instance limit (1) is reached: 409 with the count.
    let response = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/databases")))
        .header("Cookie", &auth)
        .json(&json!({ "name": "second" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "conflict");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("database limit"),
        "the refusal names its reason: {}",
        body["error"]["message"]
    );

    // A feature-off instance (limit None) refuses the same way.
    let off_instance = engine.create_instance(&cookie, None).await;
    let off_id = off_instance["id"].as_str().unwrap().to_owned();
    let response = engine
        .client
        .post(engine.url(&format!("/instances/{off_id}/databases")))
        .header("Cookie", &auth)
        .json(&json!({ "name": "anything" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    let body: Value = response.json().await.unwrap();
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not enabled"),
        "the feature-off refusal names its reason: {}",
        body["error"]["message"]
    );

    // A duplicate derived name is a 409 (names are unique across
    // every host — the Panel's own constraint).
    let twin = engine.create_instance(&cookie, Some(5)).await;
    let twin_id = twin["id"].as_str().unwrap().to_owned();
    // The derived names differ per-instance (the short uuid is the
    // namespace), so a true duplicate needs the SAME instance —
    // which the limit (1) already blocks. Bump the limit via the
    // instance PATCH (§8: the ETag rides If-Match) and retry.
    let current: Value = engine
        .client
        .get(engine.url(&format!("/instances/{instance_id}")))
        .header("Cookie", &auth)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let etag = current["etag"].as_str().unwrap().to_owned();
    let response = engine
        .client
        .patch(engine.url(&format!("/instances/{instance_id}")))
        .header("Cookie", &auth)
        .header("If-Match", etag)
        .json(&json!({ "database_limit": 5 }))
        .send()
        .await
        .unwrap();
    if response.status() != 200 {
        let status = response.status();
        let body: Value = response.json().await.unwrap();
        panic!("the limit patch failed: {status} {body}");
    }
    let response = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/databases")))
        .header("Cookie", &auth)
        .json(&json!({ "name": "first" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    let body: Value = response.json().await.unwrap();
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("already exists"),
        "the duplicate refusal names its reason: {}",
        body["error"]["message"]
    );

    // A missing host is a 404; a bad name grammar is a 422.
    let response = engine
        .client
        .post(engine.url(&format!("/instances/{twin_id}/databases")))
        .header("Cookie", &auth)
        .json(&json!({ "name": "x", "host_id": uuid::Uuid::new_v4().to_string() }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);

    let response = engine
        .client
        .post(engine.url(&format!("/instances/{twin_id}/databases")))
        .header("Cookie", &auth)
        .json(&json!({ "name": "bad name!" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "validation_failed");
}

#[tokio::test]
async fn host_delete_refuses_while_databases_reference_it() {
    let engine = TestEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let cookie = engine.login("admin@zim.local", "admin pass").await;
    let host = engine.create_host(&cookie, "anchored-mysql").await;
    let instance = engine.create_instance(&cookie, Some(5)).await;
    let instance_id = instance["id"].as_str().unwrap().to_owned();
    let auth = format!("zim_session={cookie}");

    let response = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/databases")))
        .header("Cookie", &auth)
        .json(&json!({ "name": "anchored" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);

    // The host delete is a 409 naming the live count.
    let hid = host["id"].as_str().unwrap().to_owned();
    let response = engine
        .client
        .delete(engine.url(&format!("/hosts/{hid}")))
        .header("Cookie", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    let body: Value = response.json().await.unwrap();
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("1 database"),
        "the refusal names the count: {}",
        body["error"]["message"]
    );

    // The host list surfaces the count the gate reads.
    let list: Value = engine
        .client
        .get(engine.url("/hosts"))
        .header("Cookie", &auth)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["data"][0]["databases_count"], 1);

    // Once the database is gone, the host deletes cleanly.
    let databases: Value = engine
        .client
        .get(engine.url(&format!("/instances/{instance_id}/databases")))
        .header("Cookie", &auth)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let dbid = databases["data"][0]["id"].as_str().unwrap().to_owned();
    let response = engine
        .client
        .delete(engine.url(&format!("/instances/{instance_id}/databases/{dbid}")))
        .header("Cookie", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    let response = engine
        .client
        .delete(engine.url(&format!("/hosts/{hid}")))
        .header("Cookie", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
}

#[tokio::test]
async fn database_authz_matrix_and_cross_instance_uniform_404() {
    let engine = TestEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let cookie = engine.login("admin@zim.local", "admin pass").await;
    engine.create_host(&cookie, "authz-mysql").await;
    let instance = engine.create_instance(&cookie, Some(5)).await;
    let instance_id = instance["id"].as_str().unwrap().to_owned();
    let auth = format!("zim_session={cookie}");

    // Seed one database.
    let response = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/databases")))
        .header("Cookie", &auth)
        .json(&json!({ "name": "protected" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let dbid = response.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // A subuser with ONLY database.read can list and read but not
    // create, rotate, delete, or view the password.
    let member = engine.seed_user("member@zim.local", "member pass", false);
    let member_cookie = engine.login("member@zim.local", "member pass").await;
    let member_auth = format!("zim_session={member_cookie}");
    let instance_uuid: InstanceId = instance_id.parse().unwrap();
    engine
        .persistence
        .put_permission(&zim_engine_domain::Permission {
            user_id: member.id,
            instance_id: instance_uuid,
            grants: {
                let mut grants = zim_engine_domain::PermissionSet::new();
                grants.grant(zim_engine_domain::PermissionGrant::DatabaseRead);
                grants
            },
        })
        .unwrap();

    let response = engine
        .client
        .get(engine.url(&format!("/instances/{instance_id}/databases")))
        .header("Cookie", &member_auth)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "database.read lists");
    let body: Value = response.json().await.unwrap();
    assert!(body["data"][0].get("password").is_none());

    let response = engine
        .client
        .get(engine.url(&format!("/instances/{instance_id}/databases/{dbid}")))
        .header("Cookie", &member_auth)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    for (label, method, path) in [
        (
            "create",
            "post",
            format!("/instances/{instance_id}/databases"),
        ),
        (
            "rotate",
            "post",
            format!("/instances/{instance_id}/databases/{dbid}/rotate-password"),
        ),
        (
            "delete",
            "delete",
            format!("/instances/{instance_id}/databases/{dbid}"),
        ),
        (
            "view_password",
            "get",
            format!("/instances/{instance_id}/databases/{dbid}/password"),
        ),
    ] {
        let response = match method {
            "post" => engine
                .client
                .post(engine.url(&path))
                .header("Cookie", &member_auth)
                .json(&json!({ "name": "intruder" }))
                .send()
                .await
                .unwrap(),
            "delete" => engine
                .client
                .delete(engine.url(&path))
                .header("Cookie", &member_auth)
                .send()
                .await
                .unwrap(),
            _ => engine
                .client
                .get(engine.url(&path))
                .header("Cookie", &member_auth)
                .send()
                .await
                .unwrap(),
        };
        assert_eq!(
            response.status(),
            403,
            "the {label} leg must refuse without its grant"
        );
    }

    // Upgrading with view_password (but not update) opens exactly
    // the password read — the grant's own gate, nothing wider.
    engine
        .persistence
        .put_permission(&zim_engine_domain::Permission {
            user_id: member.id,
            instance_id: instance_uuid,
            grants: {
                let mut grants = zim_engine_domain::PermissionSet::new();
                grants.grant(zim_engine_domain::PermissionGrant::DatabaseRead);
                grants.grant(zim_engine_domain::PermissionGrant::DatabaseViewPassword);
                grants
            },
        })
        .unwrap();
    let response = engine
        .client
        .get(engine.url(&format!(
            "/instances/{instance_id}/databases/{dbid}/password"
        )))
        .header("Cookie", &member_auth)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "view_password opens the read");
    let response = engine
        .client
        .post(engine.url(&format!(
            "/instances/{instance_id}/databases/{dbid}/rotate-password"
        )))
        .header("Cookie", &member_auth)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403, "view_password is not update");

    // Cross-instance ids are the uniform 404 (no existence leak).
    let stranger = engine.create_instance(&cookie, Some(5)).await;
    let stranger_id = stranger["id"].as_str().unwrap().to_owned();
    let stranger_auth = format!("zim_session={cookie}");
    let response = engine
        .client
        .get(engine.url(&format!("/instances/{stranger_id}/databases/{dbid}")))
        .header("Cookie", &stranger_auth)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        404,
        "another instance's database is a 404"
    );
}

#[tokio::test]
async fn host_selection_prefers_the_node_tie_and_the_host_cap_refuses() {
    let engine = TestEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let cookie = engine.login("admin@zim.local", "admin pass").await;
    let auth = format!("zim_session={cookie}");
    let node_id = engine.persistence.list_nodes().unwrap()[0].id.to_string();

    // A node-tied host and an untied one; the instance is on the
    // node, so the tied host wins without being named.
    let tied: Value = engine
        .client
        .post(engine.url("/hosts"))
        .header("Cookie", &auth)
        .json(&json!({
            "name": "tied-mysql",
            "host": "tied.internal",
            "port": 3306,
            "username": "admin",
            "password": "secret",
            "node_id": node_id,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let _untied: Value = engine
        .client
        .post(engine.url("/hosts"))
        .header("Cookie", &auth)
        .json(&json!({
            "name": "any-mysql",
            "host": "any.internal",
            "port": 3307,
            "username": "admin",
            "password": "secret",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let instance = engine.create_instance(&cookie, Some(5)).await;
    let instance_id = instance["id"].as_str().unwrap().to_owned();
    let response = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/databases")))
        .header("Cookie", &auth)
        .json(&json!({ "name": "placed" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let database: Value = response.json().await.unwrap();
    assert_eq!(
        database["host_id"], tied["id"],
        "the node-tied host is preferred"
    );
    assert_eq!(database["connections"]["port"], 3306);

    // The host's own cap refuses with the count (cap 1 via a fresh
    // capped host; the explicit host_id selects it).
    let capped: Value = engine
        .client
        .post(engine.url("/hosts"))
        .header("Cookie", &auth)
        .json(&json!({
            "name": "capped-mysql",
            "host": "capped.internal",
            "port": 3308,
            "username": "admin",
            "password": "secret",
            "max_databases": 1,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let capped_id = capped["id"].as_str().unwrap().to_owned();
    let response = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/databases")))
        .header("Cookie", &auth)
        .json(&json!({ "name": "on-capped", "host_id": capped_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let response = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/databases")))
        .header("Cookie", &auth)
        .json(&json!({ "name": "over-capped", "host_id": capped_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    let body: Value = response.json().await.unwrap();
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("capacity"),
        "the host-cap refusal names its reason: {}",
        body["error"]["message"]
    );
}
