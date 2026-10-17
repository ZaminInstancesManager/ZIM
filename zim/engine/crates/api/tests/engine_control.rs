//! Socket-level integration tests for the M27 engine-control plane
//! and the AD-29 attribution hardening.
//!
//! Each test boots a private engine (in-memory SQLite + generated
//! loopback certificate + ephemeral HTTPS port) and speaks real TLS:
//!
//! - the token gate (`POST /engine/control`: missing/wrong token,
//!   unknown action, the real `shutdown` and the handle it fires),
//! - the health surface's `version` field (the running engine's
//!   build version),
//! - the addon attribution middleware: a believed `X-ZIM-Addon`
//!   (installed + enabled, e.g. the bundled audit-log viewer) rides
//!   the audit record's metadata next to the authorizing user, a
//!   spoofed or unknown id is dropped — never recorded as fact —
//!   and the user agent stops being `null` on API-originated
//!   records.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use zim_engine_api::control::ControlPlane;
use zim_engine_api::tls::{generate_loopback_certificate, serve_https, TlsMaterial};
use zim_engine_api::{ApiConfig, AppState};
use zim_engine_application::ports::Persistence;
use zim_engine_application::supervision::Supervisor;
use zim_engine_auth::{ApiKeyManager, SessionManager};
use zim_engine_domain::User;

/// A running test engine with its control plane exposed.
struct TestEngine {
    base: String,
    client: reqwest::Client,
    control: ControlPlane,
    persistence: Arc<dyn Persistence>,
}

impl TestEngine {
    async fn start() -> Self {
        let persistence: Arc<dyn Persistence> =
            Arc::new(zim_engine_persistence_sqlite::SqlitePersistence::open_in_memory().unwrap());
        persistence.migrate().unwrap();

        let supervisor = Self::supervisor_over(Arc::clone(&persistence)).await;
        let data_root = std::env::temp_dir().join("zim-control-test");
        let jobs = zim_engine_jobs::JobSystem::new(
            Arc::clone(&persistence),
            None,
            zim_engine_jobs::JobPolicy {
                poll_interval: Duration::from_millis(20),
                cleaner_interval: Duration::from_millis(50),
                max_concurrent: 2,
                shutdown_grace: Duration::from_secs(5),
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
            std::env::temp_dir().join(format!("zim-control-test-{}", uuid::Uuid::new_v4()));
        let frontend = Arc::new(zim_engine_frontend_loader::FrontendService::new(
            data_dir.clone(),
            semver::Version::new(0, 0, 1),
            "v1.0.0",
        ));
        // The bundled addons seed here — the audit-log viewer is
        // installed AND enabled, which is exactly what a believed
        // attribution needs to be tested against.
        let extensions = Arc::new(zim_engine_extensions::ExtensionsService::new(
            Arc::clone(&persistence),
            Arc::clone(&frontend),
            &data_dir,
            semver::Version::new(0, 0, 1),
            "v1.0.0",
        ));
        let themes = Arc::new(zim_engine_extensions::ThemeService::new(&data_dir));

        let control = ControlPlane::new(zim_engine_api::control::ControlToken::generate());
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
            Arc::clone(&frontend),
            Arc::clone(&extensions),
            Arc::clone(&themes),
            control.clone(),
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
        Self {
            base: format!("https://127.0.0.1:{port}/api/v1"),
            client,
            control,
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

    async fn login(&self, email: &str, password: &str) -> Option<String> {
        let response = self
            .client
            .post(self.url("/auth/login"))
            .json(&json!({ "email": email, "password": password }))
            .send()
            .await
            .unwrap();
        response
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .and_then(|raw| raw.to_str().ok())
            .and_then(|raw| raw.strip_prefix("zim_session="))
            .map(|token| token.split(';').next().unwrap().to_owned())
    }

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
                monitor_poll_interval: Duration::from_millis(50),
                ..Default::default()
            },
            std::env::temp_dir().join("zim-control-test"),
        );
        supervisor.load_local().await.unwrap();
        supervisor
    }
}

// ── The control endpoint ──────────────────────────────────────────

#[tokio::test]
async fn control_without_a_token_is_refused_without_firing() {
    let engine = TestEngine::start().await;
    let handle = engine.control.shutdown_handle();
    let response = engine
        .client
        .post(engine.url("/engine/control"))
        .json(&json!({ "action": "shutdown" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "unauthorized");

    // The refusal is a refusal: nothing fired.
    tokio::time::timeout(Duration::from_millis(250), handle.notified())
        .await
        .expect_err("no shutdown may fire from an unauthenticated call");
}

#[tokio::test]
async fn control_with_a_wrong_token_is_refused_without_firing() {
    let engine = TestEngine::start().await;
    let handle = engine.control.shutdown_handle();
    let response = engine
        .client
        .post(engine.url("/engine/control"))
        .header(
            "X-ZIM-Engine-Token",
            zim_engine_api::control::ControlToken::generate().as_base64(),
        )
        .json(&json!({ "action": "shutdown" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);

    tokio::time::timeout(Duration::from_millis(250), handle.notified())
        .await
        .expect_err("no shutdown may fire from a wrong token");
}

#[tokio::test]
async fn control_with_a_malformed_token_is_refused() {
    let engine = TestEngine::start().await;
    let response = engine
        .client
        .post(engine.url("/engine/control"))
        .header("X-ZIM-Engine-Token", "!!!not-base64!!!")
        .json(&json!({ "action": "shutdown" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn control_unknown_action_is_a_validation_failure_not_a_shutdown() {
    let engine = TestEngine::start().await;
    let handle = engine.control.shutdown_handle();
    let response = engine
        .client
        .post(engine.url("/engine/control"))
        .header("X-ZIM-Engine-Token", engine.control.token_base64())
        .json(&json!({ "action": "self-destruct" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 422);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "validation_failed");

    tokio::time::timeout(Duration::from_millis(250), handle.notified())
        .await
        .expect_err("no shutdown may fire from an unknown action");
}

#[tokio::test]
async fn control_shutdown_fires_the_handle_and_audits_the_act() {
    let engine = TestEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let admin_cookie = engine
        .login("admin@zim.local", "admin pass")
        .await
        .expect("admin login");

    let response = engine
        .client
        .post(engine.url("/engine/control"))
        .header("X-ZIM-Engine-Token", engine.control.token_base64())
        .header("User-Agent", "zim_shell/0.1.0 (update coordination)")
        .json(&json!({ "action": "shutdown" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["status"], "draining");

    // The handle fires (the engine's run loop would now drain).
    tokio::time::timeout(
        Duration::from_secs(2),
        engine.control.shutdown_handle().notified(),
    )
    .await
    .expect("the shutdown handle fires");

    // The act is audited like any other privileged operation: actor
    // system, the machine-local provenance in the metadata, the
    // coordinator's user agent.
    let audit_response = engine
        .client
        .get(engine.url("/audit?limit=5"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(audit_response.status(), 200);
    let audit: Value = audit_response.json().await.unwrap();
    let records = audit["data"].as_array().expect("audit data array");
    let record = records
        .iter()
        .find(|r| r["action"] == "engine.control.shutdown")
        .expect("the engine-control shutdown is audited");
    assert_eq!(record["actor"], "system");
    assert_eq!(record["metadata"]["via"], "engine_control");
    assert_eq!(
        record["user_agent"].as_str().map(str::to_owned),
        Some("zim_shell/0.1.0 (update coordination)".to_owned())
    );
}

// ── The health surface's version ──────────────────────────────────

#[tokio::test]
async fn health_reports_the_running_engine_version() {
    let engine = TestEngine::start().await;
    let response = engine
        .client
        .get(engine.url("/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    // The RUNNING engine's version — the same value the process
    // would print for `zim_engine --version`, by construction.
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
}

// ── The addon attribution (AD-29's hardening) ─────────────────────

#[tokio::test]
async fn believed_addon_attribution_rides_the_audit_record() {
    let engine = TestEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let admin_cookie = engine
        .login("admin@zim.local", "admin pass")
        .await
        .expect("admin login");

    // One audited action with the bundled audit-log viewer's id
    // stamped by the vendored SDK's fetch wrapper.
    let response = engine
        .client
        .post(engine.url("/users"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .header("X-ZIM-Addon", "org.zaminmc.extension.audit-log")
        .header("User-Agent", "zim-addon-sdk/1.0")
        .json(&json!({
            "email": "attributed@zim.local",
            "username": "attributed",
            "password": "a password",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);

    let audit: Value = engine
        .client
        .get(engine.url("/audit?limit=10"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let records = audit["data"].as_array().expect("audit data array");
    let record = records
        .iter()
        .find(|r| r["action"] == "user.create" && r["metadata"]["addon_id"].is_string())
        .expect("the user creation carries the believed addon id");
    assert_eq!(
        record["metadata"]["addon_id"],
        "org.zaminmc.extension.audit-log"
    );
    // Attribution, not re-attribution: the actor is still the user
    // whose credentials authorized the call.
    assert_eq!(record["actor"], "user");
    assert_eq!(record["user_agent"], "zim-addon-sdk/1.0");
}

#[tokio::test]
async fn spoofed_addon_attribution_is_dropped_not_recorded() {
    let engine = TestEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let admin_cookie = engine
        .login("admin@zim.local", "admin pass")
        .await
        .expect("admin login");

    // A plausible-but-not-installed id: the engine must not record
    // it as fact (any page on the origin can set any header).
    let response = engine
        .client
        .post(engine.url("/users"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .header("X-ZIM-Addon", "com.example.attacker-addon")
        .json(&json!({
            "email": "spoofed@zim.local",
            "username": "spoofed",
            "password": "a password",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);

    let audit: Value = engine
        .client
        .get(engine.url("/audit?limit=10"))
        .header("Cookie", format!("zim_session={admin_cookie}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let records = audit["data"].as_array().expect("audit data array");
    let record = records
        .iter()
        .find(|r| {
            r["action"] == "user.create" && r["metadata"]["target_email"] == "spoofed@zim.local"
        })
        .or_else(|| records.iter().find(|r| r["action"] == "user.create"))
        .expect("the user creation is audited");
    assert!(
        record["metadata"]["addon_id"].is_null(),
        "a spoofed addon id must not ride the audit record: {}",
        record["metadata"]
    );
    assert_eq!(record["actor"], "user");
}
