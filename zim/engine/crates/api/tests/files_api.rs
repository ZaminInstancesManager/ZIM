//! Socket-level tests for the M28 file-API family.
//!
//! Each test boots a private engine (in-memory SQLite, mock runtime,
//! generated loopback certificate, ephemeral HTTPS port, temp data
//! dir) and drives the real routes end-to-end: the grant table, the
//! §2.6 path grammar refusals, the atomic upload round-trip, rename,
//! the archive/extract pair, the §13 upload channel (bodies past the
//! router-wide 10 MB JSON limit), and the audit trail.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use serde_json::{json, Value};
use zim_engine_api::tls::{generate_loopback_certificate, serve_https, TlsMaterial};
use zim_engine_api::{ApiConfig, AppState};
use zim_engine_application::ports::Persistence;
use zim_engine_application::supervision::Supervisor;
use zim_engine_auth::{ApiKeyManager, SessionManager};
use zim_engine_domain::{Permission, PermissionGrant, PermissionSet, User};

/// A running test engine plus its client (the api_surface harness,
/// trimmed to what the file family needs).
struct TestEngine {
    base: String,
    client: reqwest::Client,
    persistence: Arc<dyn Persistence>,
}

impl TestEngine {
    /// Boot an engine whose data dir is a fresh temp directory (the
    /// file family roots every operation under it).
    async fn start() -> Self {
        let persistence: Arc<dyn Persistence> =
            Arc::new(zim_engine_persistence_sqlite::SqlitePersistence::open_in_memory().unwrap());
        persistence.migrate().unwrap();
        let config = ApiConfig {
            data_dir: std::env::temp_dir().join(format!("zim-files-api-{}", uuid::Uuid::new_v4())),
            ..ApiConfig::default()
        };
        Self::start_on(config, Arc::clone(&persistence)).await
    }

    async fn start_on(config: ApiConfig, persistence: Arc<dyn Persistence>) -> Self {
        let supervisor = Self::supervisor_over(Arc::clone(&persistence)).await;
        let data_root = std::env::temp_dir().join("zim-files-api-jobs");
        let jobs = zim_engine_jobs::JobSystem::new(
            Arc::clone(&persistence),
            None,
            zim_engine_jobs::JobPolicy {
                poll_interval: std::time::Duration::from_millis(20),
                cleaner_interval: std::time::Duration::from_millis(50),
                max_concurrent: 2,
                shutdown_grace: std::time::Duration::from_secs(5),
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
        let data_dir = config.data_dir.clone();
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
            // M30: the telemetry service (history query; retention is
            // main's job, not the test engine's).
            zim_engine_telemetry::TelemetryService::new(
                Arc::clone(&persistence),
                Default::default(),
            ),
            Arc::clone(&frontend),
            Arc::clone(&extensions),
            Arc::clone(&themes),
            zim_engine_api::control::ControlPlane::for_tests(),
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
            persistence,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// A supervisor over the mock runtime factory (instances created
    /// through the API land on the seeded local node).
    async fn supervisor_over(persistence: Arc<dyn Persistence>) -> Arc<Supervisor> {
        use zim_engine_application::supervision::testing::{MockRuntime, MockRuntimeFactory};
        use zim_engine_application::supervision::SupervisionPolicy;

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
            persistence,
            Arc::new(MockRuntimeFactory::new(MockRuntime::always_running())),
            SupervisionPolicy {
                monitor_poll_interval: std::time::Duration::from_millis(50),
                ..Default::default()
            },
            std::env::temp_dir().join("zim-files-api"),
        );
        supervisor.load_local().await.unwrap();
        supervisor
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

    /// Grant a member exactly one permission row on one instance.
    fn seed_grant(
        &self,
        user: &User,
        instance_id: &zim_engine_domain::InstanceId,
        grants: &[PermissionGrant],
    ) {
        let mut set = PermissionSet::new();
        for grant in grants {
            set.grant(*grant);
        }
        self.persistence
            .put_permission(&Permission {
                user_id: user.id,
                instance_id: *instance_id,
                grants: set,
            })
            .unwrap();
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

/// Create an instance as the admin and return its id: a fresh node
/// with one free allocation, then the instance on it (the
/// api_surface suite's seeding shape — instance creation
/// auto-picks the node's free allocation).
async fn create_instance(engine: &TestEngine, cookie: &str, name: &str) -> String {
    let node: Value = engine
        .client
        .post(engine.url("/nodes"))
        .header("cookie", format!("zim_session={cookie}"))
        .json(&json!({ "name": format!("{name}-node"), "fqdn": "localhost" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let node_id = node["id"].as_str().unwrap().to_owned();
    let response = engine
        .client
        .post(engine.url(&format!("/nodes/{node_id}/allocations")))
        .header("cookie", format!("zim_session={cookie}"))
        .json(&json!({ "port": 25565 }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);

    let instance: Value = engine
        .client
        .post(engine.url("/instances"))
        .header("cookie", format!("zim_session={cookie}"))
        .json(&json!({ "name": name, "node_id": node_id }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    instance["id"].as_str().unwrap().to_owned()
}

/// One authenticated request with the session cookie attached.
fn auth(
    engine: &TestEngine,
    cookie: &str,
    method: reqwest::Method,
    path: &str,
) -> reqwest::RequestBuilder {
    engine
        .client
        .request(method, engine.url(path))
        .header("cookie", format!("zim_session={cookie}"))
}

// ── The happy-path family ────────────────────────────────────────

#[tokio::test]
async fn the_full_file_family_round_trips_over_the_real_routes() {
    let engine = TestEngine::start().await;
    engine.seed_user("admin@zim.local", "correct horse battery staple", true);
    let cookie = login(&engine, "admin@zim.local", "correct horse battery staple")
        .await
        .expect("admin login");

    let instance_id = create_instance(&engine, &cookie, "files-target").await;
    let files = format!("/instances/{instance_id}/files");

    // A fresh instance lists an empty root (the engine creates the
    // data dir on first touch — never a 500 for a never-started
    // instance).
    let listing: Value = auth(&engine, &cookie, reqwest::Method::GET, &files)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listing["entries"].as_array().unwrap().len(), 0);
    assert_eq!(listing["path"], ".");

    // Upload (PUT content) — atomic write, parents created.
    let put: Value = auth(
        &engine,
        &cookie,
        reqwest::Method::PUT,
        &format!("{files}/content?path=eula.txt"),
    )
    .body(Vec::from(b"eula=true\n"))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(put["size"], 10);

    // The listing sees it, with the honest shape.
    let listing: Value = auth(&engine, &cookie, reqwest::Method::GET, &files)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let entries = listing["entries"].as_array().unwrap().clone();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "eula.txt");
    assert_eq!(entries[0]["is_dir"], false);
    assert_eq!(entries[0]["size"], 10);
    assert!(entries[0]["modified"].is_string());

    // Download (GET content) — raw bytes with the download headers.
    let response = auth(
        &engine,
        &cookie,
        reqwest::Method::GET,
        &format!("{files}/content?path=eula.txt"),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["content-type"],
        "application/octet-stream"
    );
    assert!(response.headers()["content-disposition"]
        .to_str()
        .unwrap()
        .contains("eula.txt"));
    let bytes = response.bytes().await.unwrap();
    assert_eq!(&bytes[..], b"eula=true\n");

    // Create a directory, then a nested file through the upload path.
    let made: Value = auth(
        &engine,
        &cookie,
        reqwest::Method::POST,
        &format!("{files}/create"),
    )
    .json(&json!({ "path": "plugins", "is_dir": true }))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(made["is_dir"], true);
    let nested = auth(
        &engine,
        &cookie,
        reqwest::Method::PUT,
        &format!("{files}/content?path=plugins/config.yml"),
    )
    .body(Vec::from(b"debug: false\n"))
    .send()
    .await
    .unwrap();
    assert_eq!(nested.status(), 200);

    // List the subdirectory.
    let listing: Value = auth(
        &engine,
        &cookie,
        reqwest::Method::GET,
        &format!("{files}?path=plugins"),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(listing["path"], "plugins");
    assert_eq!(listing["entries"].as_array().unwrap().len(), 1);

    // Rename a file.
    let renamed: Value = auth(
        &engine,
        &cookie,
        reqwest::Method::POST,
        &format!("{files}/rename"),
    )
    .json(&json!({
        "old_path": "plugins/config.yml",
        "new_path": "plugins/config.bak.yml"
    }))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(renamed["new_path"], "plugins/config.bak.yml");

    // Archive the whole tree, then extract it back.
    let archived: Value = auth(
        &engine,
        &cookie,
        reqwest::Method::POST,
        &format!("{files}/archive"),
    )
    .json(&json!({
        "paths": ["eula.txt", "plugins"],
        "destination": "snapshot.tar.gz",
        "format": "tar.gz",
    }))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(archived["format"], "tar.gz");
    assert!(archived["bytes"].as_u64().unwrap() > 0);

    let extracted: Value = auth(
        &engine,
        &cookie,
        reqwest::Method::POST,
        &format!("{files}/extract"),
    )
    .json(&json!({
        "archive_path": "snapshot.tar.gz",
        "destination": "restored"
    }))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert!(extracted["entries"].as_u64().unwrap() >= 3);

    // The restored copy carries the same bytes.
    let response = auth(
        &engine,
        &cookie,
        reqwest::Method::GET,
        &format!("{files}/content?path=restored/eula.txt"),
    )
    .send()
    .await
    .unwrap();
    let bytes = response.bytes().await.unwrap();
    assert_eq!(&bytes[..], b"eula=true\n");

    // Delete the restored tree (directories recursively).
    let status = auth(
        &engine,
        &cookie,
        reqwest::Method::DELETE,
        &format!("{files}?path=restored"),
    )
    .send()
    .await
    .unwrap()
    .status();
    assert_eq!(status, 204);

    // The deleted path is honestly gone.
    let status = auth(
        &engine,
        &cookie,
        reqwest::Method::GET,
        &format!("{files}/content?path=restored/eula.txt"),
    )
    .send()
    .await
    .unwrap()
    .status();
    assert_eq!(status, 404);

    // The audit trail carries every mutation with the path and the
    // transport marker.
    let audit = engine.persistence.list_audit(None, 500).unwrap();
    let actions: Vec<&str> = audit.iter().map(|r| r.action.as_str()).collect();
    for expected in [
        "instance.file.write",
        "instance.file.create",
        "instance.file.rename",
        "instance.file.archive",
        "instance.file.extract",
        "instance.file.delete",
    ] {
        assert!(
            actions.contains(&expected),
            "missing audit action {expected}"
        );
    }
    let write = audit
        .iter()
        .find(|r| r.action == "instance.file.write")
        .expect("a write audit row");
    assert_eq!(write.metadata["transport"], "http");
    assert_eq!(write.metadata["path"], "eula.txt");
}

// ── The §2.6 grammar ────────────────────────────────────────────

#[tokio::test]
async fn the_path_grammar_is_refused_with_structured_validation_errors() {
    let engine = TestEngine::start().await;
    engine.seed_user("admin@zim.local", "correct horse battery staple", true);
    let cookie = login(&engine, "admin@zim.local", "correct horse battery staple")
        .await
        .expect("admin login");
    let instance_id = create_instance(&engine, &cookie, "grammar-target").await;
    let files = format!("/instances/{instance_id}/files");

    for hostile in ["../escape.txt", "a/../../etc", "/etc/passwd"] {
        let encoded = hostile
            .replace('%', "%25")
            .replace('/', "%2F")
            .replace("..", "%2E%2E");
        let response = auth(
            &engine,
            &cookie,
            reqwest::Method::GET,
            &format!("{files}/content?path={encoded}"),
        )
        .send()
        .await
        .unwrap();
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        assert!(
            status == 400 || status == 404,
            "path {hostile:?} -> {status} ({body})"
        );
        if status == 400 {
            assert_eq!(body["error"]["code"], "validation_failed");
            assert_eq!(body["error"]["field"], "path");
        }
    }

    // The root itself cannot be deleted (no empty relative form in
    // the grammar — the data dir is structurally safe).
    let response = auth(
        &engine,
        &cookie,
        reqwest::Method::DELETE,
        &format!("{files}?path=."),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), 400);

    // A missing ?path= on mutations is a validation error, not a 500.
    let response = auth(&engine, &cookie, reqwest::Method::DELETE, &files)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "validation_failed");
}

// ── The grant table ──────────────────────────────────────────────

#[tokio::test]
async fn the_grant_table_governs_every_route() {
    let engine = TestEngine::start().await;
    engine.seed_user("admin@zim.local", "correct horse battery staple", true);
    let member = engine.seed_user("member@zim.local", "watchword-target", false);
    engine.seed_user("stranger@zim.local", "watchword-target", false);
    let admin_cookie = login(&engine, "admin@zim.local", "correct horse battery staple")
        .await
        .expect("admin login");
    let instance_id = create_instance(&engine, &admin_cookie, "grants-target").await;
    let typed = instance_id
        .parse::<zim_engine_domain::InstanceId>()
        .unwrap();

    // Read-only member: list passes, every mutation 403s with the
    // missing grant named.
    engine.seed_grant(
        &member,
        &typed,
        &[PermissionGrant::FileRead, PermissionGrant::FileReadContent],
    );
    let member_cookie = login(&engine, "member@zim.local", "watchword-target")
        .await
        .expect("member login");
    let files = format!("/instances/{instance_id}/files");

    let status = auth(&engine, &member_cookie, reqwest::Method::GET, &files)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, 200);

    let response = auth(
        &engine,
        &member_cookie,
        reqwest::Method::PUT,
        &format!("{files}/content?path=member.txt"),
    )
    .body(Vec::from(b"no"))
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), 403);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "permission_denied");
    assert_eq!(body["error"]["missing_permissions"][0], "file.write");

    // A stranger without any row gets the uniform 404 (existence is
    // not leaked).
    let stranger_cookie = login(&engine, "stranger@zim.local", "watchword-target")
        .await
        .expect("stranger login");
    let status = auth(&engine, &stranger_cookie, reqwest::Method::GET, &files)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, 404);
}

// ── The §13 upload channel ──────────────────────────────────────

#[tokio::test]
async fn uploads_ride_their_own_body_limit_channel() {
    let engine = TestEngine::start().await;
    engine.seed_user("admin@zim.local", "correct horse battery staple", true);
    let cookie = login(&engine, "admin@zim.local", "correct horse battery staple")
        .await
        .expect("admin login");
    let instance_id = create_instance(&engine, &cookie, "upload-target").await;
    let files = format!("/instances/{instance_id}/files");

    // 11 MB: past the router-wide 10 MB JSON limit, comfortably
    // inside the route's §13 channel — proves the route-level
    // DefaultBodyLimit wins for this pair.
    let payload = vec![0xA5u8; 11 * 1024 * 1024];
    let response = auth(
        &engine,
        &cookie,
        reqwest::Method::PUT,
        &format!("{files}/content?path=big.bin"),
    )
    .body(payload)
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["size"], 11 * 1024 * 1024);

    // The JSON routes keep their own ceiling (the channel is
    // upload-shaped, not a global raise) — a small create still
    // works after the big upload.
    let response = auth(
        &engine,
        &cookie,
        reqwest::Method::POST,
        &format!("{files}/create"),
    )
    .json(&json!({ "path": "x.txt" }))
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), 201);
}
