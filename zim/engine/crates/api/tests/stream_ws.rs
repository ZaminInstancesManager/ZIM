//! WebSocket-level integration tests for `/api/v1/stream` (M7,
//! ZIM_API_SPEC.md §11).
//!
//! Each test boots a private engine over plain HTTP (the WS
//! handshake path is TLS-agnostic; the production listener's TLS is
//! exercised in `api_surface.rs`) with the scripted mock runtime,
//! then drives a real WebSocket client through the milestone's
//! acceptance scenarios:
//!
//! 1. A subscriber receives `instance.state_change` events on
//!    lifecycle transitions.
//! 2. A subscriber that disconnects and reconnects with
//!    `?since_generation={n}` receives the events it missed.
//! 3. Handshake auth (session cookie, tab token, anonymous
//!    rejection), sub-protocol negotiation, and the §10.3 console
//!    and metrics read endpoints over the hub's retained state.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;

use zim_engine_api::{ApiConfig, AppState};
use zim_engine_application::ports::Persistence;
use zim_engine_application::supervision::Supervisor;
use zim_engine_auth::{ApiKeyManager, SessionManager, TabTokenManager};
use zim_engine_domain::User;

/// The concrete socket type these tests drive.
type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// A running test engine serving the API over plain HTTP (the
/// WebSocket tests do not need TLS).
struct StreamEngine {
    base: String,
    ws_base: String,
    client: reqwest::Client,
    state: Arc<AppState>,
    persistence: Arc<dyn Persistence>,
    /// Abort the server task when the engine drops (each test owns
    /// its runtime; the tokio test runtime tears the rest down).
    _server: tokio::task::JoinHandle<()>,
}

impl StreamEngine {
    async fn start() -> Self {
        let persistence: Arc<dyn Persistence> =
            Arc::new(zim_engine_persistence_sqlite::SqlitePersistence::open_in_memory().unwrap());
        persistence.migrate().unwrap();

        let supervisor = supervisor_over(Arc::clone(&persistence)).await;
        let tab_tokens = Arc::new(TabTokenManager::new(Default::default()));
        // M8: the durable-job executor + backup service (a fast test
        // policy; the hub-less executor keeps these stream tests
        // focused on supervision events).
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
            std::env::temp_dir().join("zim-stream-test"),
        );
        backups.register().await;
        // M8.2: the transfer service (the §10.9 mutation family) —
        // registered for parity with the production assembly.
        let transfers = zim_engine_transfers::TransferService::new(
            Arc::clone(&persistence),
            Arc::clone(&jobs),
            Arc::clone(&supervisor),
            std::env::temp_dir().join("zim-stream-test"),
        );
        transfers.register().await;
        jobs.start();
        let state = Arc::new(AppState::new(
            Arc::clone(&persistence),
            supervisor,
            Arc::new(SessionManager::new(Default::default())),
            Arc::new(ApiKeyManager::new()),
            Arc::clone(&tab_tokens),
            Arc::clone(&jobs),
            Arc::clone(&backups),
            Arc::clone(&transfers),
            ApiConfig::default(),
        ));
        let router =
            zim_engine_api::router(Arc::clone(&state)).layer(axum::middleware::map_request(
                |mut req: axum::http::Request<axum::body::Body>| async move {
                    // The production TLS accept loop stamps the peer
                    // address into every request's extensions (see
                    // tls.rs's ConnectInfoService); the plain test
                    // server stamps a synthetic loopback one.
                    // Per-test engines isolate the login limiter, so
                    // the bucket key is irrelevant here.
                    req.extensions_mut()
                        .insert(SocketAddr::from(([127, 0, 0, 1], 0)));
                    req
                },
            ));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        let client = reqwest::Client::new();
        Self {
            base: format!("http://127.0.0.1:{}/api/v1", addr.port()),
            ws_base: format!("ws://127.0.0.1:{}/api/v1", addr.port()),
            client,
            state,
            persistence,
            _server: server,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn ws_url(&self, path: &str) -> String {
        format!("{}{path}", self.ws_base)
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

/// The supervisor over the mock runtime (no Docker dependency), with
/// a fast monitor so metrics and lifecycle events arrive quickly.
async fn supervisor_over(persistence: Arc<dyn Persistence>) -> Arc<Supervisor> {
    use zim_engine_application::supervision::testing::{MockRuntime, MockRuntimeFactory};
    use zim_engine_application::supervision::SupervisionPolicy;

    let nodes = persistence.list_nodes().unwrap();
    if nodes.is_empty() {
        let node = zim_engine_domain::Node {
            id: zim_engine_domain::NodeId::new(),
            name: "local-stream-test".to_owned(),
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
        std::env::temp_dir().join("zim-stream-test"),
    );
    supervisor.load_local().await.unwrap();
    supervisor
}

/// Login and return the session cookie.
async fn login(engine: &StreamEngine, email: &str, password: &str) -> String {
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

/// Seed node + one free allocation + one instance; returns the
/// instance id.
async fn seed_instance(engine: &StreamEngine, cookie: &str) -> String {
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
            "name": "stream-test-server",
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

/// Build a WS upgrade request with extra headers.
fn ws_request(
    engine: &StreamEngine,
    query: &str,
    headers: &[(&'static str, String)],
) -> tokio_tungstenite::tungstenite::http::Request<()> {
    let url = if query.is_empty() {
        engine.ws_url("/stream")
    } else {
        engine.ws_url(&format!("/stream?{query}"))
    };
    let mut request = url
        .into_client_request()
        .expect("a ws:// URL converts into an upgrade request");
    for (name, value) in headers {
        request.headers_mut().insert(
            *name,
            HeaderValue::from_str(value).expect("header value is ascii"),
        );
    }
    request
}

/// Await the next text frame as parsed JSON (timeout-bounded so a
/// broken server fails the test instead of hanging it). The engine's
/// keepalive pings are acknowledged and skipped.
async fn next_json(stream: &mut Ws) -> Value {
    let deadline = Duration::from_secs(5);
    loop {
        let message = tokio::time::timeout(deadline, stream.next())
            .await
            .expect("a frame arrives within the deadline")
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

/// Read envelopes until one matches `predicate` (deadline-bounded;
/// returns the matched envelope).
async fn until<F>(stream: &mut Ws, mut predicate: F) -> Option<Value>
where
    F: FnMut(&Value) -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        let envelope = next_json(stream).await;
        if predicate(&envelope) {
            return Some(envelope);
        }
    }
    None
}

// ── Acceptance criterion 1: lifecycle events reach subscribers ────

#[tokio::test]
async fn subscriber_receives_state_change_events_on_lifecycle() {
    let engine = StreamEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let cookie = login(&engine, "admin@zim.local", "admin pass").await;
    let instance_id = seed_instance(&engine, &cookie).await;

    // Connect before starting the instance.
    let request = ws_request(
        &engine,
        &format!("instance_id={instance_id}"),
        &[("cookie", format!("zim_session={cookie}"))],
    );
    let (mut stream, response) = connect_async(request).await.expect("upgrade succeeds");
    assert_eq!(
        response.status(),
        101,
        "the handshake completes as a protocol switch"
    );

    // Start via the HTTP API; the events must arrive on the socket.
    let response = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/start")))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);

    let starting = until(&mut stream, |env| {
        env["type"] == "instance.state_change" && env["data"]["to"] == "starting"
    })
    .await
    .expect("the offline→starting transition arrives");
    // §11.1 envelope shape on the wire.
    assert_eq!(
        starting["generation"], 1,
        "the first lane event is generation 1"
    );
    assert_eq!(starting["instance_id"], instance_id);
    assert_eq!(starting["data"]["from"], "offline");
    assert!(starting["ts"].as_str().is_some_and(|ts| ts.ends_with('Z')));

    let running = until(&mut stream, |env| {
        env["type"] == "instance.state_change" && env["data"]["to"] == "running"
    })
    .await
    .expect("the starting→running transition arrives");
    assert!(
        running["generation"].as_u64().unwrap() > starting["generation"].as_u64().unwrap(),
        "generations increase per instance"
    );

    let _ = stream.close(None).await;
}

// ── Acceptance criterion 2: replay-from-checkpoint on reconnect ───

#[tokio::test]
async fn reconnect_replays_the_missed_events() {
    let engine = StreamEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let cookie = login(&engine, "admin@zim.local", "admin pass").await;
    let instance_id = seed_instance(&engine, &cookie).await;

    // First connection: watch the start, remember the running
    // transition's generation, then disconnect.
    let request = ws_request(
        &engine,
        &format!("instance_id={instance_id}"),
        &[("cookie", format!("zim_session={cookie}"))],
    );
    let (mut stream, _) = connect_async(request).await.expect("first upgrade");
    let start = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/start")))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 202);

    let checkpoint = until(&mut stream, |env| {
        env["type"] == "instance.state_change" && env["data"]["to"] == "running"
    })
    .await
    .expect("running transition observed before disconnect")["generation"]
        .as_u64()
        .unwrap();
    drop(stream);

    // Events happen while the subscriber is disconnected.
    let stop = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/stop")))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(stop.status(), 202);
    // Let the stop's transitions land in the lane.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Reconnect with the checkpoint: the missed transitions replay.
    let request = ws_request(
        &engine,
        &format!("instance_id={instance_id}&since_generation={checkpoint}"),
        &[("cookie", format!("zim_session={cookie}"))],
    );
    let (mut stream, _) = connect_async(request).await.expect("reconnect upgrade");

    let stopping = until(&mut stream, |env| {
        env["type"] == "instance.state_change" && env["data"]["to"] == "stopping"
    })
    .await
    .expect("the missed running→stopping transition replays");
    assert!(
        stopping["generation"].as_u64().unwrap() > checkpoint,
        "replayed events are past the checkpoint"
    );

    let offline = until(&mut stream, |env| {
        env["type"] == "instance.state_change" && env["data"]["to"] == "offline"
    })
    .await
    .expect("the missed stopping→offline transition replays");
    assert!(
        offline["generation"].as_u64().unwrap() > stopping["generation"].as_u64().unwrap(),
        "replay preserves generation order"
    );
    let _ = stream.close(None).await;
}

// ── Handshake auth ────────────────────────────────────────────────

#[tokio::test]
async fn anonymous_connections_are_rejected_before_upgrade() {
    let engine = StreamEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let cookie = login(&engine, "admin@zim.local", "admin pass").await;
    let instance_id = seed_instance(&engine, &cookie).await;

    let request = ws_request(&engine, &format!("instance_id={instance_id}"), &[]);
    let error = connect_async(request)
        .await
        .expect_err("anonymous upgrades are rejected");
    assert_eq!(
        http_status_of(&error),
        401,
        "the structured 401 precedes the upgrade"
    );
}

#[tokio::test]
async fn tab_tokens_authorize_the_stream() {
    let engine = StreamEngine::start().await;
    let admin = engine.seed_user("admin@zim.local", "admin pass", true);
    let cookie = login(&engine, "admin@zim.local", "admin pass").await;
    let instance_id = seed_instance(&engine, &cookie).await;
    let instance_uuid: zim_engine_domain::InstanceId =
        instance_id.parse().expect("instance ids are UUIDs");

    // A tab token scoped to the instance with the connect grant.
    let mut grants = zim_engine_domain::PermissionSet::new();
    grants.grant(zim_engine_domain::PermissionGrant::WebsocketConnect);
    let issued = engine.state.tab_tokens.issue(
        admin.id,
        instance_uuid,
        grants.clone(),
        zim_engine_auth::new_tab_id(),
        chrono::Utc::now(),
    );

    let request = ws_request(
        &engine,
        &format!("instance_id={instance_id}"),
        &[("x-zim-tab-token", issued.token.clone())],
    );
    let (mut stream, _) = connect_async(request).await.expect("tab token upgrades");
    let start = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/start")))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 202);
    let first = until(&mut stream, |env| env["type"] == "instance.state_change")
        .await
        .expect("tab-token subscribers receive events");
    assert_eq!(first["generation"], 1);
    let _ = stream.close(None).await;

    // A token scoped to a different instance is refused.
    let other = zim_engine_domain::InstanceId::new();
    let other_token = engine.state.tab_tokens.issue(
        admin.id,
        other,
        grants,
        zim_engine_auth::new_tab_id(),
        chrono::Utc::now(),
    );
    let request = ws_request(
        &engine,
        &format!("instance_id={instance_id}"),
        &[("x-zim-tab-token", other_token.token)],
    );
    let error = connect_async(request)
        .await
        .expect_err("cross-instance tab tokens are rejected");
    assert_eq!(http_status_of(&error), 403);
}

#[tokio::test]
async fn unknown_subprotocol_is_rejected() {
    let engine = StreamEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let cookie = login(&engine, "admin@zim.local", "admin pass").await;
    let instance_id = seed_instance(&engine, &cookie).await;

    let request = ws_request(
        &engine,
        &format!("instance_id={instance_id}"),
        &[
            ("cookie", format!("zim_session={cookie}")),
            ("sec-websocket-protocol", "bogus.v9".to_owned()),
        ],
    );
    let error = connect_async(request)
        .await
        .expect_err("unsupported sub-protocols are rejected");
    assert_eq!(http_status_of(&error), 400);

    // The supported sub-protocol negotiates and is echoed.
    let request = ws_request(
        &engine,
        &format!("instance_id={instance_id}"),
        &[
            ("cookie", format!("zim_session={cookie}")),
            ("sec-websocket-protocol", "zim-events.v1".to_owned()),
        ],
    );
    let (_stream, response) = connect_async(request).await.expect("v1 negotiates");
    let echoed = response
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert_eq!(echoed, "zim-events.v1");
}

#[tokio::test]
async fn missing_or_malformed_query_is_a_structured_400() {
    let engine = StreamEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let cookie = login(&engine, "admin@zim.local", "admin pass").await;
    let instance_id = seed_instance(&engine, &cookie).await;

    // Missing instance_id.
    let request = ws_request(&engine, "", &[("cookie", format!("zim_session={cookie}"))]);
    let error = connect_async(request)
        .await
        .expect_err("instance_id is required");
    assert_eq!(http_status_of(&error), 400);

    // Malformed since_generation.
    let request = ws_request(
        &engine,
        &format!("instance_id={instance_id}&since_generation=not-a-number"),
        &[("cookie", format!("zim_session={cookie}"))],
    );
    let error = connect_async(request)
        .await
        .expect_err("since_generation must parse");
    assert_eq!(http_status_of(&error), 400);
}

// ── The §10.3 console/metrics reads over the retained hub state ──

#[tokio::test]
async fn console_and_metrics_endpoints_serve_hub_state() {
    let engine = StreamEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let cookie = login(&engine, "admin@zim.local", "admin pass").await;
    let instance_id = seed_instance(&engine, &cookie).await;

    // Start and let the monitor tick a few times (metrics events).
    let start = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/start")))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 202);
    tokio::time::sleep(Duration::from_millis(250)).await;

    // Metrics carries the monitor's resource-usage samples.
    let response = engine
        .client
        .get(engine.url(&format!("/instances/{instance_id}/metrics")))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    let samples = body["samples"].as_array().expect("samples array");
    assert!(
        !samples.is_empty(),
        "the monitor's ticks produce resource-usage samples"
    );
    assert_eq!(samples[0]["type"], "instance.resource_usage");
    assert!(samples[0]["data"]["memory_usage_bytes"].as_u64().is_some());
    assert!(body["last_generation"].as_u64().unwrap() > 0);

    // Console buffer: the M7.1 attach pump keeps the echoed input
    // lines (the live path, no daemon required) and serves the
    // reconnect cursor.
    let response = engine
        .client
        .get(engine.url(&format!("/instances/{instance_id}/console")))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert!(body["last_generation"].as_u64().unwrap() > 0);
    assert!(
        body["events"].as_array().unwrap().is_empty(),
        "no console input yet: the buffer starts empty"
    );

    // Console input reaches the runtime's attachment and answers
    // the sent ack (M7.1: the echo console implements the live
    // path; the pre-M7.1 501 is retired).
    let response = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/console/input")))
        .header("Cookie", format!("zim_session={cookie}"))
        .json(&json!({ "line": "say hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["status"], "sent");
    assert_eq!(body["bytes"].as_u64().unwrap(), "say hello".len() as u64);

    // The echoed line lands in the retained console buffer (the
    // supervision pump publishes asynchronously — poll briefly).
    let echoed = std::time::Instant::now();
    let events = loop {
        let response = engine
            .client
            .get(engine.url(&format!("/instances/{instance_id}/console")))
            .header("Cookie", format!("zim_session={cookie}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await.unwrap();
        let events = body["events"].as_array().expect("events array").clone();
        if !events.is_empty() || echoed.elapsed() > Duration::from_secs(2) {
            break events;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert_eq!(events.len(), 1, "exactly one echoed console line");
    assert_eq!(events[0]["type"], "instance.console_output");
    assert_eq!(events[0]["data"]["line"], "say hello\n");
    assert_eq!(events[0]["instance_id"], instance_id);

    // Validation still applies before the runtime is consulted.
    let response = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/console/input")))
        .header("Cookie", format!("zim_session={cookie}"))
        .json(&json!({ "line": 42 }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
}

// ── M7.1: the live console path over the WebSocket ────────────────

/// The §M7.1 acceptance flow at the API level (echo-console mock):
/// a WS subscriber receives `instance.console_output` lines in
/// generation order, console input surfaces on the stream, and a
/// reconnect with `?since_generation` replays the retained console
/// tail.
#[tokio::test]
async fn ws_console_output_live_and_replay() {
    let engine = StreamEngine::start().await;
    engine.seed_user("admin@zim.local", "admin pass", true);
    let cookie = login(&engine, "admin@zim.local", "admin pass").await;
    let instance_id = seed_instance(&engine, &cookie).await;

    // Subscribe, then start the instance.
    let request = ws_request(
        &engine,
        &format!("instance_id={instance_id}"),
        &[("cookie", format!("zim_session={cookie}"))],
    );
    let (mut stream, _response) = connect_async(request).await.expect("upgrade succeeds");

    let start = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/start")))
        .header("Cookie", format!("zim_session={cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 202);

    // Wait for the running transition (the console attach arms with
    // it), then send console input over the HTTP endpoint.
    until(&mut stream, |env| {
        env["type"] == "instance.state_change" && env["data"]["to"] == "running"
    })
    .await
    .expect("the starting→running transition arrives");

    let input = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/console/input")))
        .header("Cookie", format!("zim_session={cookie}"))
        .json(&json!({ "line": "say hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(input.status(), 200);

    // The echoed line arrives as an instance.console_output envelope
    // in generation order (after any earlier lane events).
    let console = until(&mut stream, |env| {
        env["type"] == "instance.console_output"
            && env["data"]["line"]
                .as_str()
                .is_some_and(|l| l == "say hello\n")
    })
    .await
    .expect("the echoed line reaches the subscriber");
    assert_eq!(console["instance_id"], instance_id);
    assert!(console["generation"].as_u64().unwrap() > 0);
    let checkpoint = console["generation"].as_u64().unwrap();

    // A second line after the checkpoint: §11.3 replays exactly the
    // events newer than the checkpoint, so this one (not the first)
    // is what a reconnecting subscriber must receive.
    let input = engine
        .client
        .post(engine.url(&format!("/instances/{instance_id}/console/input")))
        .header("Cookie", format!("zim_session={cookie}"))
        .json(&json!({ "line": "list players" }))
        .send()
        .await
        .unwrap();
    assert_eq!(input.status(), 200);

    let second = until(&mut stream, |env| {
        env["type"] == "instance.console_output"
            && env["data"]["line"]
                .as_str()
                .is_some_and(|l| l == "list players\n")
    })
    .await
    .expect("the second echoed line reaches the subscriber");
    assert!(
        second["generation"].as_u64().unwrap() > checkpoint,
        "the second line is stamped after the checkpoint"
    );

    let _ = stream.close(None).await;

    // Reconnect from the checkpoint: the retained console tail newer
    // than the checkpoint is replayed (§11.3) — the second line, at
    // its original generation; the first (already-delivered,
    // generation == checkpoint) is not re-sent.
    let request = ws_request(
        &engine,
        &format!("instance_id={instance_id}&since_generation={checkpoint}"),
        &[("cookie", format!("zim_session={cookie}"))],
    );
    let (mut stream, _response) = connect_async(request).await.expect("reconnect succeeds");

    let replayed = until(&mut stream, |env| {
        env["type"] == "instance.console_output"
            && env["data"]["line"]
                .as_str()
                .is_some_and(|l| l == "list players\n")
    })
    .await
    .expect("the retained console tail replays after reconnect");
    assert_eq!(
        replayed["generation"].as_u64().unwrap(),
        second["generation"].as_u64().unwrap(),
        "the replayed line keeps its original generation"
    );

    let _ = stream.close(None).await;
}

// ── Helpers ───────────────────────────────────────────────────────

/// Extract the HTTP status from a failed handshake (tungstenite
/// surfaces non-101 responses as `Error::Http`).
fn http_status_of(error: &tokio_tungstenite::tungstenite::Error) -> u16 {
    match error {
        tokio_tungstenite::tungstenite::Error::Http(response) => response.status().as_u16(),
        other => panic!("expected an HTTP handshake rejection, got {other:?}"),
    }
}
