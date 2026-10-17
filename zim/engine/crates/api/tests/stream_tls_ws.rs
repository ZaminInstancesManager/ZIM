//! WebSocket-level regression tests for `/api/v1/stream` over the
//! PRODUCTION TLS accept loop (M25 batch 16).
//!
//! `stream_ws.rs` drives the plain-HTTP `axum::serve` path — which
//! enables HTTP upgrades by default — and so could never see the
//! defect these tests pin: `tls.rs`'s manual accept loop used
//! hyper-util's plain `serve_connection`, which completes the 101
//! exchange but never hands the IO to the upgrade extension, so the
//! live event stream dropped every subscriber the instant they
//! connected (the browser saw 1006 and reconnect-loopoped forever).
//! Found by M25's E2E browser suite; this file keeps it found.
//!
//! The client here mirrors the production shape as closely as a
//! test can: a real TLS connection (danger verifier — the engine's
//! loopback certificate is self-signed by design), a real
//! `wss://` upgrade with the session cookie and the
//! `zim-events.v1` sub-protocol, and the mock runtime's lifecycle
//! events as the first frames the socket must deliver.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_rustls::rustls;
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use zim_engine_api::tls::{generate_loopback_certificate, serve_https, RunningServer};
use zim_engine_api::{ApiConfig, AppState};
use zim_engine_application::ports::Persistence;
use zim_engine_application::supervision::Supervisor;
use zim_engine_auth::{ApiKeyManager, SessionManager, TabTokenManager};
use zim_engine_domain::User;

/// The concrete socket type these tests drive: a tungstenite stream
/// over a tokio-rustls client connection.
type Ws = WebSocketStream<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>;

/// A running engine serving the API over the production TLS accept
/// loop on an ephemeral loopback port.
struct TlsEngine {
    base: String,
    ws_base: String,
    client: reqwest::Client,
    persistence: Arc<dyn Persistence>,
    server: RunningServer,
}

impl TlsEngine {
    async fn start() -> Self {
        let persistence: Arc<dyn Persistence> =
            Arc::new(zim_engine_persistence_sqlite::SqlitePersistence::open_in_memory().unwrap());
        persistence.migrate().unwrap();

        let supervisor = supervisor_over(Arc::clone(&persistence)).await;
        let tab_tokens = Arc::new(TabTokenManager::new(Default::default()));
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
        let data_dir = std::env::temp_dir().join(format!("zim-tls-ws-{}", uuid::Uuid::new_v4()));
        let backups = zim_engine_backups::BackupService::new(
            Arc::clone(&persistence),
            Arc::clone(&jobs),
            data_dir.clone(),
        );
        backups.register().await;
        let transfers = zim_engine_transfers::TransferService::new(
            Arc::clone(&persistence),
            Arc::clone(&jobs),
            Arc::clone(&supervisor),
            data_dir.clone(),
        );
        transfers.register().await;
        let installs = zim_engine_installs::InstallService::new(
            Arc::clone(&persistence),
            Arc::clone(&jobs),
            Arc::clone(&supervisor),
            Arc::new(
                zim_engine_application::supervision::testing::MockInstallRunner::always_succeeds(),
            ),
            data_dir.clone(),
        );
        installs.register().await;
        jobs.start();
        let scheduler =
            zim_engine_scheduler::Scheduler::new(Arc::clone(&persistence), Arc::clone(&supervisor));
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
            Arc::clone(&tab_tokens),
            Arc::clone(&jobs),
            Arc::clone(&backups),
            Arc::clone(&transfers),
            Arc::clone(&installs),
            Arc::clone(&scheduler),
            Arc::clone(&frontend),
            Arc::clone(&extensions),
            Arc::clone(&themes),
            ApiConfig::default(),
        ));
        let router = zim_engine_api::router(Arc::clone(&state));

        // The production listener: self-signed loopback certificate,
        // the manual accept loop from tls.rs (the code under test).
        let tls = generate_loopback_certificate().expect("loopback certificate");
        let server = serve_https(router, std::net::IpAddr::from([127, 0, 0, 1]), 0, &tls)
            .await
            .expect("the TLS server binds");
        let port = server.local_addr.port();

        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .expect("danger client builds (self-signed loopback cert by design)");
        Self {
            base: format!("https://127.0.0.1:{port}/api/v1"),
            ws_base: format!("wss://127.0.0.1:{port}/api/v1"),
            client,
            persistence,
            server,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn ws_url(&self, path: &str) -> String {
        format!("{}{path}", self.ws_base)
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
}

/// The supervisor over the mock runtime (no Docker dependency), with
/// a fast monitor so lifecycle events arrive quickly.
async fn supervisor_over(persistence: Arc<dyn Persistence>) -> Arc<Supervisor> {
    use zim_engine_application::supervision::testing::{MockRuntime, MockRuntimeFactory};
    use zim_engine_application::supervision::SupervisionPolicy;

    let nodes = persistence.list_nodes().unwrap();
    if nodes.is_empty() {
        let node = zim_engine_domain::Node {
            id: zim_engine_domain::NodeId::new(),
            name: "local-tls-stream-test".to_owned(),
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
        Arc::new(MockRuntimeFactory::new(
            MockRuntime::always_running().with_echo_console(),
        )),
        SupervisionPolicy {
            monitor_poll_interval: Duration::from_millis(50),
            ..Default::default()
        },
        std::env::temp_dir().join("zim-tls-stream-test"),
    );
    supervisor.load_local().await.unwrap();
    supervisor
}

/// Login over the TLS API and return the session cookie.
async fn login(engine: &TlsEngine, email: &str, password: &str) -> String {
    let response = engine
        .client
        .post(engine.url("/auth/login"))
        .json(&json!({ "email": email, "password": password }))
        .send()
        .await
        .unwrap();
    if response.status() != 200 {
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        panic!("login failed: {status} {body}");
    }
    response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .find_map(|v| {
            let raw = v.to_str().ok()?;
            let token = raw.strip_prefix("zim_session=")?.split(';').next()?;
            Some(token.to_owned())
        })
        .expect("login sets a session cookie")
}

/// Seed node + one free allocation + one instance over the TLS API.
async fn seed_instance(engine: &TlsEngine, cookie: &str) -> String {
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

    let response = engine
        .client
        .post(engine.url("/instances"))
        .header("Cookie", format!("zim_session={cookie}"))
        .json(&json!({
            "name": "tls-stream-test-server",
            "node_id": node["id"].as_str().unwrap(),
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
    instance["id"].as_str().unwrap().to_owned()
}

// ── The danger verifier: the engine's certificate IS self-signed ──

/// Accepts the engine's self-signed loopback certificate (the
/// production shell installs it into the OS trust store; the test
/// client skips that ceremony — the certificate's self-signed nature
/// is by design, `tls.rs` §loopback).
#[derive(Debug)]
struct AcceptLoopback;

impl rustls::client::danger::ServerCertVerifier for AcceptLoopback {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

/// Open the TLS connection and perform the `wss://` upgrade with the
/// session cookie and the `zim-events.v1` sub-protocol — the exact
/// production handshake shape.
async fn tls_websocket(engine: &TlsEngine, instance_id: &str, cookie: &str) -> Ws {
    let url = engine.ws_url(&format!("/stream?instance_id={instance_id}"));
    let mut request = url
        .into_client_request()
        .expect("a wss:// URL converts into an upgrade request");
    request.headers_mut().insert(
        "cookie",
        HeaderValue::from_str(&format!("zim_session={cookie}")).expect("cookie is ascii"),
    );
    request.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static("zim-events.v1"),
    );

    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptLoopback))
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));

    let port = engine
        .ws_base
        .rsplit(':')
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .expect("the ws base carries a port");
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("the engine port accepts TCP");
    let server_name = ServerName::from(std::net::IpAddr::from([127, 0, 0, 1]));
    let tls_stream = connector
        .connect(server_name, tcp)
        .await
        .expect("the TLS handshake completes against the loopback certificate");

    let (socket, response) = tokio_tungstenite::client_async(request, tls_stream)
        .await
        .expect("the wss:// upgrade completes over the production accept loop");
    assert_eq!(response.status(), 101, "the handshake is a protocol switch");
    socket
}

/// Await the next text frame as parsed JSON (timeout-bounded so a
/// broken server fails the test instead of hanging it); the engine's
/// keepalive pings are acknowledged and skipped.
async fn next_json(stream: &mut Ws) -> Value {
    let deadline = Duration::from_secs(5);
    loop {
        let message = tokio::time::timeout(deadline, stream.next())
            .await
            .expect("a frame arrives within the deadline — with the plain serve_connection bug the socket dies here instead (1006, zero frames)")
            .expect("the socket is open")
            .expect("the frame is well-formed");
        match message {
            Message::Text(text) => {
                return serde_json::from_str(text.as_str()).expect("envelopes are JSON")
            }
            Message::Ping(payload) => {
                let _ = stream.send(Message::Pong(payload)).await;
            }
            other => panic!("expected a text envelope, got {other:?}"),
        }
    }
}

// ── The regression: live frames over the production TLS server ────

/// The M25 batch-16 regression: a `wss://` subscriber over the REAL
/// accept loop receives the lifecycle's live events. With hyper-util's
/// plain `serve_connection` the upgrade answered 101 and the socket
/// dropped before the first frame; `serve_connection_with_upgrades`
/// hands the IO to the session, and the frames flow.
#[tokio::test]
async fn wss_stream_delivers_live_events_over_the_production_tls_loop() {
    let engine = TlsEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let cookie = login(&engine, "admin@zim.local", "admin pass").await;
    let instance_id = seed_instance(&engine, &cookie).await;

    // Subscribe BEFORE starting — the first frames must be the
    // offline→starting→running transitions themselves.
    let mut stream = tls_websocket(&engine, &instance_id, &cookie).await;

    let start = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/start")))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 202);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut saw_starting = false;
    let mut saw_running = false;
    while tokio::time::Instant::now() < deadline && !saw_running {
        let envelope = next_json(&mut stream).await;
        if envelope["type"] != "instance.state_change" {
            continue;
        }
        match envelope["data"]["to"].as_str().unwrap_or("") {
            "starting" => saw_starting = true,
            "running" => saw_running = true,
            _ => {}
        }
    }
    assert!(
        saw_starting,
        "the offline→starting transition arrived over the TLS socket"
    );
    assert!(
        saw_running,
        "the starting→running transition arrived over the TLS socket"
    );

    let _ = stream.close(None).await;
    engine.server.shutdown().await;
}

/// The console input echo round-trip over TLS: input POSTed over
/// HTTPS surfaces as an `instance.console_output` frame on the
/// `wss://` socket (the mock runtime echoes).
#[tokio::test]
async fn wss_stream_carries_console_output_over_the_production_tls_loop() {
    let engine = TlsEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let cookie = login(&engine, "admin@zim.local", "admin pass").await;
    let instance_id = seed_instance(&engine, &cookie).await;

    let mut stream = tls_websocket(&engine, &instance_id, &cookie).await;

    let start = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/start")))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 202);

    let input = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/console/input")))
        .header("Cookie", format!("zim_session={cookie}"))
        .json(&json!({ "line": "hello over tls" }))
        .send()
        .await
        .unwrap();
    assert_eq!(input.status(), 202);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut echoed = false;
    while tokio::time::Instant::now() < deadline && !echoed {
        let envelope = next_json(&mut stream).await;
        if envelope["type"] == "instance.console_output"
            && envelope["data"]["line"]
                .as_str()
                .is_some_and(|line| line.contains("hello over tls"))
        {
            echoed = true;
        }
    }
    assert!(
        echoed,
        "the console echo arrived as a live frame over the TLS socket"
    );

    let _ = stream.close(None).await;
    engine.server.shutdown().await;
}
