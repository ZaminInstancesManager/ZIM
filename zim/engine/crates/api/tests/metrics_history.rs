//! Socket-level tests for the M30 metrics history endpoint
//! (`GET /instances/{id}/metrics/history`).
//!
//! Each test boots a private engine whose mock runtime reports a
//! *ticking* workload (advancing cumulative CPU/network counters, so
//! the derived series carries real rates and percentages) and whose
//! supervision policy persists telemetry at a fast test cadence.
//! The walks cover: the default window, the window vocabulary, the
//! derived point shape (rates present, host-share CPU), the honest
//! empty series for an instance that never ran, the structured 400
//! for an unknown window, and the authentication gate.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use zim_engine_api::tls::{generate_loopback_certificate, serve_https, TlsMaterial};
use zim_engine_api::{ApiConfig, AppState};
use zim_engine_application::ports::Persistence;
use zim_engine_application::supervision::testing::{MockRuntime, MockRuntimeFactory};
use zim_engine_application::supervision::{SupervisionPolicy, Supervisor};
use zim_engine_auth::{ApiKeyManager, SessionManager};
use zim_engine_domain::User;

/// A running test engine over the ticking-stats mock runtime.
struct MetricsEngine {
    base: String,
    client: reqwest::Client,
    persistence: Arc<dyn Persistence>,
}

impl MetricsEngine {
    /// Boot: in-memory store, a local node, a supervisor whose mock
    /// runtime ticks its counters, and a 100 ms telemetry persist
    /// cadence so the durable series fills in test time.
    async fn start() -> Self {
        let persistence: Arc<dyn Persistence> =
            Arc::new(zim_engine_persistence_sqlite::SqlitePersistence::open_in_memory().unwrap());
        persistence.migrate().unwrap();

        // The local node the API-created instances land on.
        if persistence.list_nodes().unwrap().is_empty() {
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
            Arc::new(MockRuntimeFactory::new(
                MockRuntime::always_running().with_ticking_stats(),
            )),
            SupervisionPolicy {
                monitor_poll_interval: Duration::from_millis(25),
                metrics_persist_interval: Duration::from_millis(100),
                ..SupervisionPolicy::default()
            },
            std::env::temp_dir().join("zim-metrics-history-test"),
        );
        supervisor.load_local().await.unwrap();

        let data_root = std::env::temp_dir().join("zim-api-test");
        let jobs = zim_engine_jobs::JobSystem::new(
            Arc::clone(&persistence),
            Some(supervisor.stream().clone()),
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
            data_root,
        );
        installs.register().await;
        jobs.start();
        let scheduler =
            zim_engine_scheduler::Scheduler::new(Arc::clone(&persistence), Arc::clone(&supervisor));

        let data_dir =
            std::env::temp_dir().join(format!("zim-metrics-history-{}", uuid::Uuid::new_v4()));
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
        Self {
            base: format!("https://127.0.0.1:{port}/api/v1"),
            client,
            persistence,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Seed an admin user (Argon2id-hashed) and return the login
    /// cookie.
    async fn seed_admin_and_login(&self) -> String {
        let policy = zim_engine_auth::PasswordPolicy::default();
        let hash = zim_engine_auth::password::hash_password(&policy, "metrics pass").unwrap();
        let user = User {
            id: zim_engine_domain::UserId::new(),
            email: "admin@zim.local".to_owned(),
            username: "admin".to_owned(),
            external_id: None,
            password_hash: Some(hash),
            oauth_subject: None,
            is_admin: true,
            root_admin: true,
        };
        self.persistence.put_user(&user).unwrap();
        let response = self
            .client
            .post(self.url("/auth/login"))
            .json(&json!({ "email": "admin@zim.local", "password": "metrics pass" }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .find_map(|v| {
                let raw = v.to_str().ok()?;
                let token = raw.strip_prefix("zim_session=")?.split(';').next()?;
                Some(token.to_owned())
            })
            .expect("the login sets a session cookie")
    }

    /// Create an instance through the API (the mock runtime's node).
    async fn seed_instance(&self, cookie: &str, name: &str) -> String {
        let nodes = self
            .client
            .get(self.url("/nodes"))
            .header("Cookie", format!("zim_session={cookie}"))
            .send()
            .await
            .unwrap();
        assert_eq!(nodes.status(), 200);
        let body: Value = nodes.json().await.unwrap();
        let node_id = body["data"][0]["id"].as_str().expect("a node").to_owned();
        let response = self
            .client
            .post(self.url(&format!("/nodes/{node_id}/allocations")))
            .header("Cookie", format!("zim_session={cookie}"))
            .json(&json!({ "port": 25575 }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 201);
        let response = self
            .client
            .post(self.url("/instances"))
            .header("Cookie", format!("zim_session={cookie}"))
            .json(&json!({ "name": name, "node_id": node_id }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 201);
        let instance: Value = response.json().await.unwrap();
        instance["id"].as_str().unwrap().to_owned()
    }
}

#[tokio::test]
async fn history_serves_the_derived_series_for_a_running_instance() {
    let engine = MetricsEngine::start().await;
    let cookie = engine.seed_admin_and_login().await;
    let instance_id = engine.seed_instance(&cookie, "metrics-server").await;

    // Start: the monitor samples the ticking runtime and persists
    // at the 100 ms test cadence.
    let response = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/start")))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);

    // Wait for the durable series to fill (several persisted rows).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut points: Vec<Value>;
    loop {
        let response = engine
            .client
            .get(engine.url(&format!("/instances/{instance_id}/metrics/history")))
            .header("Cookie", format!("zim_session={cookie}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await.unwrap();
        // The default window is 1h with its documented resolution.
        assert_eq!(body["window"], json!("1h"));
        assert_eq!(body["resolution_seconds"], json!(20));
        assert_eq!(body["instance_id"], json!(instance_id));
        points = body["points"].as_array().unwrap().clone();
        if points.len() >= 3 || tokio::time::Instant::now() > deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
    assert!(
        points.len() >= 3,
        "the persisted series must fill at the test cadence, got {} points",
        points.len()
    );

    // The derived shape: the ticking workload's CPU reads a flat
    // 10 % (host-share), the network rates are positive, and the
    // memory gauges ride along. (The 10 % carries float dust —
    // 0.1 * 100 rounds a hair above — so the tolerance.)
    for point in &points {
        assert!(point["ts"].is_string(), "every point is timestamped");
        let cpu = point["cpu_percent"].as_f64().unwrap();
        assert!(
            (cpu - 10.0).abs() < 0.01,
            "cpu reads a flat 10 %, got {cpu}"
        );
        assert!(point["network_rx_bytes_per_sec"].as_f64().unwrap() > 0.0);
        assert!(point["network_tx_bytes_per_sec"].as_f64().unwrap() > 0.0);
        assert!(point["memory_usage_bytes"].as_u64().unwrap() > 0);
        assert!(point["memory_percent"].as_f64().is_some());
    }

    // The summary agrees with the points.
    let response = engine
        .client
        .get(engine.url(&format!("/instances/{instance_id}/metrics/history")))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    let body: Value = response.json().await.unwrap();
    let avg = body["summary"]["cpu_percent_avg"].as_f64().unwrap();
    assert!(
        (avg - 10.0).abs() < 0.01,
        "summary avg is the flat 10 %, got {avg}"
    );
    let max = body["summary"]["cpu_percent_max"].as_f64().unwrap();
    assert!((max - 10.0).abs() < 0.01);
    assert!(body["summary"]["memory_bytes_max"].as_u64().unwrap() > 0);
    assert!(body["summary"]["point_count"].as_u64().unwrap() >= 3);

    // Stop the instance (the engine outlives the walk).
    let response = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/stop")))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
}

#[tokio::test]
async fn the_window_vocabulary_selects_the_resolution() {
    let engine = MetricsEngine::start().await;
    let cookie = engine.seed_admin_and_login().await;
    let instance_id = engine.seed_instance(&cookie, "windows-server").await;

    for (window, resolution) in [
        ("15m", 5),
        ("1h", 20),
        ("6h", 120),
        ("24h", 480),
        ("7d", 3360),
    ] {
        let response = engine
            .client
            .get(engine.url(&format!(
                "/instances/{instance_id}/metrics/history?window={window}"
            )))
            .header("Cookie", format!("zim_session={cookie}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["window"], json!(window));
        assert_eq!(body["resolution_seconds"], json!(resolution));
        // Case-insensitive: the wire vocabulary tolerates 1H.
        if window == "1h" {
            let response = engine
                .client
                .get(engine.url(&format!(
                    "/instances/{instance_id}/metrics/history?window=1H"
                )))
                .header("Cookie", format!("zim_session={cookie}"))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            let body: Value = response.json().await.unwrap();
            assert_eq!(body["window"], json!("1h"));
        }
    }
}

#[tokio::test]
async fn an_unknown_window_is_a_structured_refusal() {
    let engine = MetricsEngine::start().await;
    let cookie = engine.seed_admin_and_login().await;
    let instance_id = engine.seed_instance(&cookie, "refused-server").await;

    let response = engine
        .client
        .get(engine.url(&format!(
            "/instances/{instance_id}/metrics/history?window=fortnight"
        )))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], json!("bad_request"));
    assert_eq!(body["error"]["field"], json!("window"));
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("15m, 1h, 6h, 24h, 7d"));
}

#[tokio::test]
async fn an_instance_that_never_ran_has_an_honestly_empty_series() {
    let engine = MetricsEngine::start().await;
    let cookie = engine.seed_admin_and_login().await;
    let instance_id = engine.seed_instance(&cookie, "idle-server").await;

    let response = engine
        .client
        .get(engine.url(&format!("/instances/{instance_id}/metrics/history")))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["points"].as_array().unwrap().len(), 0);
    assert_eq!(body["summary"]["point_count"], json!(0));
    assert_eq!(body["summary"]["first_ts"], Value::Null);
    assert_eq!(body["summary"]["cpu_percent_avg"], Value::Null);
}

#[tokio::test]
async fn history_requires_a_credential() {
    let engine = MetricsEngine::start().await;
    let cookie = engine.seed_admin_and_login().await;
    let instance_id = engine.seed_instance(&cookie, "locked-server").await;

    let response = engine
        .client
        .get(engine.url(&format!("/instances/{instance_id}/metrics/history")))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}
