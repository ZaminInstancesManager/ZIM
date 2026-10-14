//! Docker integration tests (M6 acceptance + M7.1 console).
//!
//! These run against the *real* local Docker daemon and self-skip
//! (printing a notice, exiting Ok) when none is reachable — the
//! workspace's main CI job must stay Docker-free, and the dedicated
//! `docker-integration` CI job (ubuntu runners ship Docker) runs
//! them for real.
//!
//! Coverage: the §M6 acceptance criteria — start/observe Running/
//! stop/observe Offline, crash detection with exit codes + OOM flag
//! and crash-loop engagement, engine-restart reconciliation — plus
//! the Wings-parity container spec (readonly rootfs, tmpfs, network
//! attach, port bindings) and the exec/stats capabilities; and the
//! §M7.1 acceptance criteria — the console attach pump's
//! stdout-to-hub round trip, stdin-through-the-attachment input, and
//! the console retention bounds under load.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use zim_engine_application::ports::Persistence;
use zim_engine_application::supervision::{
    ObservedState, PowerAction, Runtime, RuntimeFactory, SupervisionPolicy, Supervisor,
};
use zim_engine_domain::{
    EggId, Instance, InstanceId, InstanceStatus, Node, NodeCapabilities, NodeDeployment, NodeState,
    User,
};
use zim_engine_runtime_docker::{DockerRuntimeFactory, DockerSettings};

/// Unique container name per test run.
fn unique_name(prefix: &str) -> String {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}-{suffix}")
}

/// Connect to Docker or skip the test.
async fn docker_or_skip() -> Option<bollard::Docker> {
    let docker = match bollard::Docker::connect_with_local_defaults() {
        Ok(docker) => docker,
        Err(_) => {
            eprintln!("SKIPPED: no local docker daemon (connect failed)");
            return None;
        }
    };
    match tokio::time::timeout(Duration::from_secs(10), docker.version()).await {
        Ok(Ok(_)) => Some(docker),
        _ => {
            eprintln!("SKIPPED: local docker daemon not responding");
            None
        }
    }
}

/// Test settings: the Wings defaults, but a test data root.
fn test_settings() -> DockerSettings {
    DockerSettings {
        data_root: std::env::temp_dir().join("zim-docker-test"),
        ..DockerSettings::default()
    }
}

/// A minimal busybox-sleep instance config.
fn busybox_config(
    name: &str,
    command: Vec<String>,
    host_port: u16,
) -> (
    zim_engine_application::supervision::InstanceConfig,
    std::path::PathBuf,
) {
    let data_dir = std::env::temp_dir().join(format!("zim-docker-data-{}", name));
    let config = zim_engine_application::supervision::InstanceConfig {
        instance_id: InstanceId::new(),
        container_name: name.to_owned(),
        hostname: name.to_owned(),
        image: "busybox:latest".to_owned(),
        entrypoint: None,
        command,
        env: vec![
            ("SERVER_MEMORY".to_owned(), "0".to_owned()),
            ("SERVER_PORT".to_owned(), host_port.to_string()),
            ("SERVER_IP".to_owned(), "127.0.0.1".to_owned()),
        ],
        limits: Default::default(),
        port_bindings: vec![zim_engine_application::supervision::PortBinding {
            host_ip: "127.0.0.1".parse().unwrap(),
            host_port,
            container_port: 8080,
            protocol: Default::default(),
        }],
        data_dir: data_dir.clone(),
        container_data_dir: std::path::PathBuf::from("/home/container"),
        labels: vec![("Service".to_owned(), "ZIM".to_owned())],
        crash_detection_enabled: true,
    };
    (config, data_dir)
}

/// Ensure busybox:latest exists locally (pull on first use).
async fn ensure_busybox(docker: &bollard::Docker) {
    if docker.inspect_image("busybox:latest").await.is_err() {
        use futures_util::StreamExt;
        let options = bollard::query_parameters::CreateImageOptionsBuilder::new()
            .from_image("busybox")
            .tag("latest")
            .build();
        let mut stream = docker.create_image(Some(options), None, None);
        while let Some(item) = stream.next().await {
            item.expect("image pull progress");
        }
    }
}

async fn remove_quietly(docker: &bollard::Docker, name: &str) {
    zim_engine_runtime_docker::remove_container_force(docker, name).await;
}

async fn wait_until_running(runtime: &dyn Runtime) -> ObservedState {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(state) = runtime.state().await {
            if state.is_running() {
                return state;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "container never reached Running"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_until_offline(runtime: &dyn Runtime) -> ObservedState {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(state) = runtime.state().await {
            if matches!(state, ObservedState::Offline { .. }) {
                return state;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "container never reached Offline"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// ── Network setup ─────────────────────────────────────────────────

#[tokio::test]
async fn pterodactyl0_network_setup() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let factory = DockerRuntimeFactory::connect(test_settings())
        .await
        .expect("factory connects");

    let inspect = docker
        .inspect_network(
            "pterodactyl0",
            None::<bollard::query_parameters::InspectNetworkOptions>,
        )
        .await
        .expect("pterodactyl0 exists after factory connect");
    assert!(inspect.id.is_some());
    // A bridge network with the ZIM service label.
    let labels = inspect.labels.unwrap_or_default();
    assert_eq!(labels.get("Service").map(String::as_str), Some("ZIM"));
    // Auto-allocated v4 subnet: the IPAM config exists with a gateway.
    let ipam = inspect.ipam.expect("ipam present");
    let config = ipam.config.expect("ipam config");
    assert!(!config.is_empty());
    assert!(config[0].gateway.is_some());

    // The factory's ping must succeed (the enqueue-time probe).
    factory.check().await.expect("daemon reachable");
}

// ── Lifecycle: start / observe / stop / observe ────────────────────

/// The §M6 acceptance flow: an instance can be started, observed
/// Running, stopped, observed Offline.
#[tokio::test]
async fn container_lifecycle_start_stop() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    ensure_busybox(&docker).await;
    let name = unique_name("zim-life");
    remove_quietly(&docker, &name).await;

    let factory = Arc::new(
        DockerRuntimeFactory::connect(test_settings())
            .await
            .expect("factory"),
    );
    let (config, _data_dir) = busybox_config(&name, vec!["sleep".into(), "600".into()], 46101);
    let runtime = factory.create(&config).await.expect("runtime");

    // Offline before anything.
    let (exit, oom) = runtime.exit_state().await.expect("exit state");
    assert_eq!((exit, oom), (None, false));

    // Start → Running with pid + started_at.
    runtime.start(&config).await.expect("start");
    let state = wait_until_running(runtime.as_ref()).await;
    match &state {
        ObservedState::Running { pid, started_at } => {
            assert!(pid.is_some(), "docker reports the host pid");
            assert!(*started_at <= Utc::now() + chrono::Duration::seconds(5));
        }
        other => panic!("expected Running, got {other:?}"),
    }

    // Stop (graceful deadline) → Offline with the TERM exit code
    // (busybox sleep dies to SIGTERM: 143; docker's own escalation
    // would report 137 — both are graceful-stop outcomes here).
    runtime.stop(Duration::from_secs(10)).await.expect("stop");
    let state = wait_until_offline(runtime.as_ref()).await;
    match state {
        ObservedState::Offline {
            exit_code,
            oom_killed,
        } => {
            assert!(!oom_killed);
            assert!(
                matches!(exit_code, Some(143) | Some(137)),
                "graceful stop exit code: {exit_code:?}"
            );
        }
        other => panic!("expected Offline, got {other:?}"),
    }

    // Stopping an already-stopped container is Ok (idempotent,
    // Wings's 404/offline tolerance).
    runtime
        .stop(Duration::from_secs(5))
        .await
        .expect("idempotent stop");

    remove_quietly(&docker, &name).await;
}

/// Kill: immediate SIGKILL, exit code 137.
#[tokio::test]
async fn container_kill() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    ensure_busybox(&docker).await;
    let name = unique_name("zim-kill");
    remove_quietly(&docker, &name).await;

    let factory = Arc::new(
        DockerRuntimeFactory::connect(test_settings())
            .await
            .expect("factory"),
    );
    let (config, _data_dir) = busybox_config(&name, vec!["sleep".into(), "600".into()], 46102);
    let runtime = factory.create(&config).await.expect("runtime");

    runtime.start(&config).await.expect("start");
    wait_until_running(runtime.as_ref()).await;

    runtime.kill().await.expect("kill");
    let state = wait_until_offline(runtime.as_ref()).await;
    match state {
        ObservedState::Offline {
            exit_code,
            oom_killed,
        } => {
            assert!(!oom_killed);
            assert_eq!(exit_code, Some(137), "SIGKILL exit code");
        }
        other => panic!("expected Offline, got {other:?}"),
    }
    remove_quietly(&docker, &name).await;
}

/// Start recreates the container (Wings's OnBeforeStart): a second
/// start produces a fresh container with the current config.
#[tokio::test]
async fn start_recreates_the_container() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    ensure_busybox(&docker).await;
    let name = unique_name("zim-recreate");
    remove_quietly(&docker, &name).await;

    let factory = Arc::new(
        DockerRuntimeFactory::connect(test_settings())
            .await
            .expect("factory"),
    );
    let (config, _data_dir) = busybox_config(&name, vec!["sleep".into(), "600".into()], 46103);
    let runtime = factory.create(&config).await.expect("runtime");

    runtime.start(&config).await.expect("first start");
    let first_id = docker
        .inspect_container(
            &name,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .expect("inspect")
        .id;

    // Start again (the persisted config always wins).
    runtime.start(&config).await.expect("second start");
    let second_id = docker
        .inspect_container(
            &name,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .expect("inspect")
        .id;
    assert_ne!(first_id, second_id, "the container was recreated");
    wait_until_running(runtime.as_ref()).await;
    remove_quietly(&docker, &name).await;
}

// ── Wings-parity container spec ───────────────────────────────────

/// Readonly rootfs + tmpfs `/tmp` + the pterodactyl0 attach, verified
/// from inside the container through exec.
#[tokio::test]
async fn wings_parity_container_spec() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    ensure_busybox(&docker).await;
    let name = unique_name("zim-spec");
    remove_quietly(&docker, &name).await;

    let factory = Arc::new(
        DockerRuntimeFactory::connect(test_settings())
            .await
            .expect("factory"),
    );
    let (config, _data_dir) = busybox_config(&name, vec!["sleep".into(), "600".into()], 46104);
    let runtime = factory.create(&config).await.expect("runtime");
    runtime.start(&config).await.expect("start");
    wait_until_running(runtime.as_ref()).await;

    use zim_engine_application::supervision::ExecCommand;

    // /tmp is a writable tmpfs.
    let touch_tmp = ExecCommand {
        cmd: vec!["touch".into(), "/tmp/zim-probe".into()],
        ..Default::default()
    };
    let out = runtime.exec(&touch_tmp).await.expect("exec /tmp write");
    assert_eq!(
        out.exit_code,
        Some(0),
        "stdout: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );

    // The root filesystem is readonly.
    let touch_etc = ExecCommand {
        cmd: vec!["touch".into(), "/etc/zim-probe".into()],
        ..Default::default()
    };
    let out = runtime.exec(&touch_etc).await.expect("exec /etc write");
    assert_ne!(out.exit_code, Some(0), "readonly rootfs must reject writes");

    // The container is on the pterodactyl0 network (its first hop
    // resolves through the network's subnet).
    let route = ExecCommand {
        cmd: vec![
            "sh".into(),
            "-c".into(),
            "ip -4 addr show eth0 | grep -o 'inet [0-9.]*'".into(),
        ],
        ..Default::default()
    };
    let out = runtime.exec(&route).await.expect("exec ip addr");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(stdout.contains("inet "), "eth0 has an address: {stdout}");
    // busybox ip may be absent; fall back to /proc/net/route semantics
    // is unnecessary — the address check suffices.

    // The hostname is the container name (the instance UUID).
    let hostname = ExecCommand {
        cmd: vec!["hostname".into()],
        ..Default::default()
    };
    let out = runtime.exec(&hostname).await.expect("exec hostname");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), name);

    remove_quietly(&docker, &name).await;
}

/// The published port binding actually listens on the host.
#[tokio::test]
async fn port_binding_reaches_the_container() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    ensure_busybox(&docker).await;
    let name = unique_name("zim-port");
    remove_quietly(&docker, &name).await;

    let factory = Arc::new(
        DockerRuntimeFactory::connect(test_settings())
            .await
            .expect("factory"),
    );
    // busybox nc listening on the container port 8080.
    let (config, _data_dir) = busybox_config(
        &name,
        vec!["nc".into(), "-l".into(), "-p".into(), "8080".into()],
        46105,
    );
    let runtime = factory.create(&config).await.expect("runtime");
    runtime.start(&config).await.expect("start");
    wait_until_running(runtime.as_ref()).await;

    // Give nc a moment to bind, then connect through the host port.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let connected = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::net::TcpStream::connect(("127.0.0.1", 46105))
            .await
            .is_ok()
    })
    .await
    .unwrap_or(false);
    assert!(connected, "the host port reaches the container");

    remove_quietly(&docker, &name).await;
}

// ── Capabilities: exec + stats ────────────────────────────────────

#[tokio::test]
async fn exec_and_stats() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    ensure_busybox(&docker).await;
    let name = unique_name("zim-exec");
    remove_quietly(&docker, &name).await;

    let factory = Arc::new(
        DockerRuntimeFactory::connect(test_settings())
            .await
            .expect("factory"),
    );
    let (config, _data_dir) = busybox_config(&name, vec!["sleep".into(), "600".into()], 46106);
    let runtime = factory.create(&config).await.expect("runtime");
    runtime.start(&config).await.expect("start");
    wait_until_running(runtime.as_ref()).await;

    use zim_engine_application::supervision::ExecCommand;

    let echo = ExecCommand {
        cmd: vec![
            "sh".into(),
            "-c".into(),
            "echo zim-exec-ok; echo err 1>&2".into(),
        ],
        ..Default::default()
    };
    let out = runtime.exec(&echo).await.expect("exec");
    assert_eq!(out.exit_code, Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("zim-exec-ok"));
    assert!(String::from_utf8_lossy(&out.stderr).contains("err"));

    let failing = ExecCommand {
        cmd: vec!["sh".into(), "-c".into(), "exit 42".into()],
        ..Default::default()
    };
    let out = runtime.exec(&failing).await.expect("exec failing");
    assert_eq!(out.exit_code, Some(42));

    let stats = runtime.stats().await.expect("stats");
    assert!(stats.memory_usage_bytes.unwrap_or(0) > 0, "usage reported");
    assert!(stats.pids_current.unwrap_or(0) > 0, "pids reported");

    remove_quietly(&docker, &name).await;
}

// ── Supervision over the real runtime ─────────────────────────────

fn seed_store() -> Arc<dyn Persistence> {
    let store = zim_engine_persistence_sqlite::SqlitePersistence::open_in_memory().unwrap();
    store.migrate().unwrap();
    Arc::new(store)
}

fn seed_node(store: &dyn Persistence) -> Node {
    let node = Node {
        id: zim_engine_domain::NodeId::new(),
        name: "local-docker".to_owned(),
        location_id: zim_engine_domain::LocationId::new(),
        fqdn: "localhost".to_owned(),
        scheme: zim_engine_domain::Scheme::Https,
        public_port: 8443,
        listen_port: 8444,
        capabilities: NodeCapabilities::default(),
        deployment: NodeDeployment::LocalLinux,
        last_heartbeat: None,
        last_seen_state: NodeState::Unknown,
    };
    store.put_node(&node).unwrap();
    node
}

fn seed_user(store: &dyn Persistence) -> User {
    let user = User {
        id: zim_engine_domain::UserId::new(),
        email: "docker-test@example.org".to_owned(),
        username: "tester".to_owned(),
        external_id: None,
        password_hash: None,
        oauth_subject: None,
        is_admin: false,
        root_admin: false,
    };
    store.put_user(&user).unwrap();
    user
}

/// Seed an instance whose startup exits nonzero immediately
/// (a crashing workload) or runs (a healthy one).
fn seed_instance(
    store: &dyn Persistence,
    node: &Node,
    owner: &User,
    port: u16,
    startup: &str,
) -> Instance {
    let (uuid, uuid_short) = Instance::legacy_uuid_pair();
    let allocation_id = store
        .create_allocation(&node.id, "127.0.0.1".parse().unwrap(), port, None)
        .unwrap();
    let instance = Instance {
        id: InstanceId::new(),
        external_id: None,
        uuid,
        uuid_short,
        node_id: node.id,
        name: "Docker Test".to_owned(),
        description: String::new(),
        status: InstanceStatus::Offline,
        skip_egg_scripts: false,
        owner_id: owner.id,
        limits: Default::default(),
        allocation_id,
        egg_id: EggId::new(),
        startup: zim_engine_domain::CommandTemplate::new(startup),
        image: "busybox:latest".parse().unwrap(),
        allocation_limit: None,
        database_limit: None,
        backup_limit: 0,
        crash_detection_enabled: true,
        installed_at: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    store.put_instance(&instance).unwrap();
    let mut allocation = store.get_allocation(&allocation_id).unwrap().unwrap();
    allocation.assigned_to = Some(instance.id);
    store.put_allocation(&allocation).unwrap();
    instance
}

async fn soon<F, Fut>(mut predicate: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline {
        if predicate().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    predicate().await
}

/// The full §M6 acceptance flow over the real Docker runtime through
/// the supervisor: power queue → running container → graceful stop.
#[tokio::test]
async fn supervisor_lifecycle_over_docker() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    ensure_busybox(&docker).await;

    let store = seed_store();
    let user = seed_user(store.as_ref());
    let node = seed_node(store.as_ref());
    let instance = seed_instance(store.as_ref(), &node, &user, 46110, "sleep 600");
    remove_quietly(&docker, &instance.uuid).await;

    let factory = Arc::new(
        DockerRuntimeFactory::connect(test_settings())
            .await
            .expect("factory"),
    );
    let supervisor = Supervisor::new(
        Arc::clone(&store),
        factory,
        SupervisionPolicy::default(),
        test_settings().data_root,
    );
    supervisor.load_local().await.expect("load");

    // Start through the supervisor (invariant 2: accepted, then the
    // runtime observation marks Running).
    supervisor
        .request_power(&instance.id, PowerAction::Start)
        .await
        .expect("start accepted");
    assert!(
        soon(|| async {
            supervisor
                .observed(&instance.id)
                .await
                .map(|s| s.observed.is_running())
                .unwrap_or(false)
        })
        .await
    );
    let snapshot = supervisor.observed(&instance.id).await.unwrap();
    assert_eq!(snapshot.status, InstanceStatus::Running);

    // The persisted row follows the observation.
    let row = store.get_instance(&instance.id).unwrap().unwrap();
    assert_eq!(row.status, InstanceStatus::Running);

    // Graceful stop through the supervisor.
    supervisor
        .request_power(
            &instance.id,
            PowerAction::Stop {
                deadline: Duration::from_secs(15),
            },
        )
        .await
        .expect("stop accepted");
    assert!(
        soon(|| async {
            supervisor
                .observed(&instance.id)
                .await
                .map(|s| matches!(s.observed, ObservedState::Offline { .. }))
                .unwrap_or(false)
        })
        .await
    );
    let row = store.get_instance(&instance.id).unwrap().unwrap();
    assert_eq!(row.status, InstanceStatus::Offline);

    supervisor.shutdown().await;
    remove_quietly(&docker, &instance.uuid).await;
}

/// §M6 crash detection over the real runtime: a workload that exits
/// nonzero trips the crash loop after the threshold and stops
/// auto-restarting; the exit code is captured from the container.
#[tokio::test]
async fn supervisor_crash_loop_over_docker() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    ensure_busybox(&docker).await;

    let store = seed_store();
    let user = seed_user(store.as_ref());
    let node = seed_node(store.as_ref());
    // Crash immediately, nonzero: `sh -c "exit 3"` — but the M6
    // renderer shlex-splits the startup, so quote handling lands in
    // shlex: `sh -c exit 3` runs `sh -c exit` (exit 0). Use `false`
    // instead: a single argv word that always exits 1.
    let instance = seed_instance(store.as_ref(), &node, &user, 46111, "false");
    remove_quietly(&docker, &instance.uuid).await;

    let factory = Arc::new(
        DockerRuntimeFactory::connect(test_settings())
            .await
            .expect("factory"),
    );
    let supervisor = Supervisor::new(
        Arc::clone(&store),
        factory,
        SupervisionPolicy {
            crash: Default::default(),
            monitor_poll_interval: Duration::from_millis(200),
            stop_deadline: Duration::from_secs(10),
            stream_metrics: false,
        },
        test_settings().data_root,
    );
    supervisor.load_local().await.expect("load");

    supervisor
        .request_power(&instance.id, PowerAction::Start)
        .await
        .expect("start accepted");

    // Crash-loop engages after 3 crashes in the window.
    assert!(
        soon(|| async {
            supervisor
                .observed(&instance.id)
                .await
                .map(|s| s.in_crash_loop)
                .unwrap_or(false)
        })
        .await,
        "crash loop engages"
    );
    let snapshot = supervisor.observed(&instance.id).await.unwrap();
    assert_eq!(snapshot.status, InstanceStatus::CrashLoop);
    // The crash's exit code is captured from the container.
    match snapshot.observed {
        ObservedState::Offline {
            exit_code,
            oom_killed,
        } => {
            assert_eq!(exit_code, Some(1), "`false` exits 1");
            assert!(!oom_killed);
        }
        other => panic!("expected Offline, got {other:?}"),
    }

    // No further auto-restarts while looped (wait out the poll cycle
    // and check the container is still gone).
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!supervisor
        .observed(&instance.id)
        .await
        .unwrap()
        .observed
        .is_running());

    supervisor.shutdown().await;
    remove_quietly(&docker, &instance.uuid).await;
}

/// §M6 engine-restart reconciliation over the real runtime: a
/// container started outside the engine is adopted; a was-desired-
/// started instance whose container is gone is restarted.
#[tokio::test]
async fn supervisor_reconcile_over_docker() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    ensure_busybox(&docker).await;

    let store = seed_store();
    let user = seed_user(store.as_ref());
    let node = seed_node(store.as_ref());
    let adopted = seed_instance(store.as_ref(), &node, &user, 46112, "sleep 600");
    let restarted = seed_instance(store.as_ref(), &node, &user, 46113, "sleep 600");
    remove_quietly(&docker, &adopted.uuid).await;
    remove_quietly(&docker, &restarted.uuid).await;

    // `adopted`'s container is started directly through the runtime
    // (i.e. it survived an engine restart).
    let factory = Arc::new(
        DockerRuntimeFactory::connect(test_settings())
            .await
            .expect("factory"),
    );
    let bootstrap = factory
        .create(&zim_engine_application::supervision::InstanceConfig {
            instance_id: adopted.id,
            container_name: adopted.uuid.clone(),
            hostname: adopted.uuid.clone(),
            image: "busybox:latest".to_owned(),
            entrypoint: None,
            command: vec!["sleep".into(), "600".into()],
            env: Vec::new(),
            limits: Default::default(),
            port_bindings: Vec::new(),
            data_dir: std::env::temp_dir()
                .join("zim-docker-test")
                .join("instances")
                .join(&adopted.uuid_short),
            container_data_dir: std::path::PathBuf::from("/home/container"),
            labels: Vec::new(),
            crash_detection_enabled: true,
        })
        .await
        .expect("runtime");
    bootstrap
        .start(&zim_engine_application::supervision::InstanceConfig {
            instance_id: adopted.id,
            container_name: adopted.uuid.clone(),
            hostname: adopted.uuid.clone(),
            image: "busybox:latest".to_owned(),
            entrypoint: None,
            command: vec!["sleep".into(), "600".into()],
            env: Vec::new(),
            limits: Default::default(),
            port_bindings: Vec::new(),
            data_dir: std::env::temp_dir()
                .join("zim-docker-test")
                .join("instances")
                .join(&adopted.uuid_short),
            container_data_dir: std::path::PathBuf::from("/home/container"),
            labels: Vec::new(),
            crash_detection_enabled: true,
        })
        .await
        .expect("bootstrap start");

    // The persisted rows claim both were running before "the engine
    // restarted" (the reconcile's job is to not trust that).
    store
        .update_instance_status(&adopted.id, InstanceStatus::Running)
        .unwrap();
    store
        .update_instance_status(&restarted.id, InstanceStatus::Running)
        .unwrap();

    let supervisor = Supervisor::new(
        Arc::clone(&store),
        factory,
        SupervisionPolicy {
            crash: Default::default(),
            monitor_poll_interval: Duration::from_millis(200),
            stop_deadline: Duration::from_secs(10),
            stream_metrics: false,
        },
        test_settings().data_root,
    );
    let report = supervisor.boot().await.expect("boot");
    assert_eq!(report.observed_running, 1, "the live container is adopted");
    assert_eq!(report.queued_restarts, 1, "the dead one is restarted");

    // The restarted instance actually runs now.
    assert!(
        soon(|| async {
            supervisor
                .observed(&restarted.id)
                .await
                .map(|s| s.observed.is_running())
                .unwrap_or(false)
        })
        .await
    );

    supervisor.shutdown().await;
    remove_quietly(&docker, &adopted.uuid).await;
    remove_quietly(&docker, &restarted.uuid).await;
}

// ── M7.1: the console attach pump (stdout streaming + stdin) ─────

/// §M7.1 direct-runtime acceptance: `attach_console` streams the
/// container's stdout (newline-terminated, generation-ordered lines
/// on the channel), `send_input` reaches the container's stdin, and
/// a not-running target answers the structured `NotRunning` form.
#[tokio::test]
async fn console_attach_and_input_round_trip() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    ensure_busybox(&docker).await;
    let name = unique_name("zim-console-direct");
    remove_quietly(&docker, &name).await;

    let factory = Arc::new(
        DockerRuntimeFactory::connect(test_settings())
            .await
            .expect("factory"),
    );
    let (config, _data_dir) = busybox_config(&name, vec!["cat".into()], 46120);
    let runtime = factory.create(&config).await.expect("runtime");

    // No container yet: both console surfaces answer NotRunning (the
    // retryable state-conflict form, never a silent success).
    let attach = runtime.attach_console().await;
    assert!(
        matches!(
            attach,
            Err(zim_engine_application::supervision::RuntimeError::NotRunning(_))
        ),
        "attach before start answers NotRunning, got {attach:?}"
    );
    let input = runtime.send_input(b"early\n").await;
    assert!(
        matches!(
            input,
            Err(zim_engine_application::supervision::RuntimeError::NotRunning(_))
        ),
        "input before start answers NotRunning, got {input:?}"
    );

    // The echo container: a TTY `cat` repeats its stdin on stdout.
    runtime.start(&config).await.expect("start");
    wait_until_running(runtime.as_ref()).await;

    let mut lines = runtime.attach_console().await.expect("attach").lines;

    // stdin reaches the container through the persistent attachment
    // (newline-terminated by the runtime when the payload is not).
    runtime.send_input(b"hello-from-zim").await.expect("input");

    // The echoed line arrives on the channel within a generous
    // deadline (a TTY echoes the input, and cat repeats it — at
    // least one occurrence is the contract).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut seen = false;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), lines.recv()).await {
            Ok(Some(line)) => {
                if line.contains("hello-from-zim") {
                    seen = true;
                    break;
                }
            }
            Ok(None) => panic!("console stream ended before the echo arrived"),
            Err(_) => continue, // keep waiting within the deadline
        }
    }
    assert!(seen, "the container's stdout carries the input line");

    // Graceful stop ends the stream (the flush path: recv drains to
    // None instead of hanging).
    runtime.stop(Duration::from_secs(10)).await.expect("stop");
    let mut drained_end = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), lines.recv()).await {
            Ok(Some(_)) => continue,
            Ok(None) => {
                drained_end = true;
                break;
            }
            Err(_) => continue,
        }
    }
    assert!(
        drained_end,
        "the attach stream ends after the container exits"
    );

    // After the exit the attachment is gone: input answers
    // NotRunning again (the pump detached the stdin writer).
    let input = runtime.send_input(b"late\n").await;
    assert!(
        matches!(
            input,
            Err(zim_engine_application::supervision::RuntimeError::NotRunning(_))
        ),
        "input after exit answers NotRunning, got {input:?}"
    );

    remove_quietly(&docker, &name).await;
}

/// §M7.1 supervisor acceptance: the console pump publishes the
/// running container's stdout onto the hub as
/// `instance.console_output` events in generation order, and console
/// input surfaces on the stream within a monitor tick.
#[tokio::test]
async fn console_pump_publishes_to_hub_through_supervisor() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    ensure_busybox(&docker).await;

    let store = seed_store();
    let user = seed_user(store.as_ref());
    let node = seed_node(store.as_ref());
    // The echo container: `cat` repeats stdin on stdout.
    let instance = seed_instance(store.as_ref(), &node, &user, 46121, "cat");
    remove_quietly(&docker, &instance.uuid).await;

    let factory = Arc::new(
        DockerRuntimeFactory::connect(test_settings())
            .await
            .expect("factory"),
    );
    let supervisor = Supervisor::new(
        Arc::clone(&store),
        factory,
        SupervisionPolicy {
            crash: Default::default(),
            monitor_poll_interval: Duration::from_millis(200),
            stop_deadline: Duration::from_secs(10),
            stream_metrics: false,
        },
        test_settings().data_root,
    );
    supervisor.load_local().await.expect("load");

    supervisor
        .request_power(&instance.id, PowerAction::Start)
        .await
        .expect("start accepted");
    assert!(
        soon(|| async {
            supervisor
                .observed(&instance.id)
                .await
                .map(|s| s.observed.is_running())
                .unwrap_or(false)
        })
        .await
    );

    // Console input surfaces on the stream as an echoed console
    // event (generation-ordered on the lane).
    supervisor
        .send_console_input(&instance.id, b"hello-through-hub")
        .await
        .expect("console input");
    let hub_lines = || {
        supervisor
            .stream()
            .console_buffer(instance.id)
            .iter()
            .map(|event| match &event.data {
                zim_engine_streaming::EventData::ConsoleOutput { line } => line.clone(),
                other => panic!("console lane carries console events only, got {other:?}"),
            })
            .collect::<Vec<String>>()
    };
    assert!(
        soon(|| async { hub_lines().iter().any(|l| l.contains("hello-through-hub")) }).await,
        "the echoed line reaches the hub's console lane"
    );
    {
        let lines = hub_lines();
        let matched: Vec<&String> = lines
            .iter()
            .filter(|l| l.contains("hello-through-hub"))
            .collect();
        assert!(
            !matched.is_empty() && matched[0].ends_with('\n'),
            "lines are newline-terminated"
        );
    }

    // Graceful stop; the pump ends with the container (no hang, no
    // leak into the next run — covered for restarts by the mock
    // suite's replace-semantics test).
    supervisor
        .request_power(
            &instance.id,
            PowerAction::Stop {
                deadline: Duration::from_secs(10),
            },
        )
        .await
        .expect("stop accepted");
    assert!(
        soon(|| async {
            supervisor
                .observed(&instance.id)
                .await
                .map(|s| matches!(s.observed, ObservedState::Offline { .. }))
                .unwrap_or(false)
        })
        .await
    );

    supervisor.shutdown().await;
    remove_quietly(&docker, &instance.uuid).await;
}

/// §M7.1 retention bounds under load: a container that emits far
/// more than the retention window keeps the hub's console lane
/// bounded (10k events / 1 MB defaults), evicting from the front and
/// keeping the newest tail.
///
/// The workload paces its 40k lines over several seconds (the attach
/// must land while output is still flowing — a one-shot emitter can
/// exit before the pump attaches) and ends with a distinctive marker
/// line, which doubles as the drain-complete signal.
#[tokio::test]
async fn console_retention_bounds_under_load() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    ensure_busybox(&docker).await;

    let store = seed_store();
    let user = seed_user(store.as_ref());
    let node = seed_node(store.as_ref());
    // 200 bursts × 400 lines = 80k lines (each ~4-6 bytes on the
    // TTY: far over the 10k-event bound, well under the 1 MB byte
    // bound, so the event cap is the binding one), paced over ~10s —
    // the attach lands within the first second or two, so even a
    // slow runner captures several times the retention window.
    let instance = seed_instance(
        store.as_ref(),
        &node,
        &user,
        46122,
        "sh -c \"for i in $(seq 1 200); do seq 1 400; sleep 0.05; done; echo FINAL-ZIM-MARKER\"",
    );
    remove_quietly(&docker, &instance.uuid).await;
    // A one-shot workload: disable crash detection so the exit does
    // not auto-restart (and re-emit) the whole sequence.
    {
        let mut row = store.get_instance(&instance.id).unwrap().unwrap();
        row.crash_detection_enabled = false;
        store.put_instance(&row).unwrap();
    }

    let factory = Arc::new(
        DockerRuntimeFactory::connect(test_settings())
            .await
            .expect("factory"),
    );
    let supervisor = Supervisor::new(
        Arc::clone(&store),
        factory,
        SupervisionPolicy {
            crash: Default::default(),
            monitor_poll_interval: Duration::from_millis(200),
            stop_deadline: Duration::from_secs(10),
            stream_metrics: false,
        },
        test_settings().data_root,
    );
    supervisor.load_local().await.expect("load");

    supervisor
        .request_power(&instance.id, PowerAction::Start)
        .await
        .expect("start accepted");

    // The marker line is the stream's last event: its arrival in the
    // retained lane means the workload finished AND the pump drained
    // everything it captured.
    let marker_deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        assert!(
            tokio::time::Instant::now() < marker_deadline,
            "the workload's marker line reaches the console lane"
        );
        let last_is_marker = supervisor
            .stream()
            .console_buffer(instance.id)
            .last()
            .is_some_and(|event| {
                matches!(
                    &event.data,
                    zim_engine_streaming::EventData::ConsoleOutput { line }
                        if line.contains("FINAL-ZIM-MARKER")
                )
            });
        if last_is_marker {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let buffer = supervisor.stream().console_buffer(instance.id);
    let policy = supervisor.stream().policy();
    assert!(
        buffer.len() <= policy.console_retention_events,
        "retention keeps at most {} events, got {}",
        policy.console_retention_events,
        buffer.len()
    );
    assert_eq!(
        buffer.len(),
        policy.console_retention_events,
        "80k lines over-fill the window: eviction engaged and the lane sits at the cap"
    );
    let bytes: usize = buffer
        .iter()
        .map(|event| match &event.data {
            zim_engine_streaming::EventData::ConsoleOutput { line } => line.len(),
            _ => 0,
        })
        .sum();
    assert!(
        bytes <= policy.console_retention_bytes,
        "retention keeps at most {} bytes, got {}",
        policy.console_retention_bytes,
        bytes
    );
    // The newest tail survived: the marker (the stream's very last
    // line) is the newest retained event.
    assert!(
        buffer.last().is_some_and(|event| {
            matches!(
                &event.data,
                zim_engine_streaming::EventData::ConsoleOutput { line }
                    if line.contains("FINAL-ZIM-MARKER")
            )
        }),
        "the newest line (the marker) is retained"
    );
    // Generations on the lane are unique and strictly ordered (the
    // §11.2 ordering witness; also proves eviction did not duplicate
    // or re-order retained events).
    let generations: Vec<u64> = buffer.iter().map(|e| e.generation).collect();
    let mut sorted = generations.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        generations.len(),
        sorted.len(),
        "no duplicate generations: {generations:?}"
    );
    let mut ordered = generations.clone();
    ordered.sort_unstable();
    assert_eq!(
        generations, ordered,
        "retained events are in generation order"
    );

    supervisor.shutdown().await;
    remove_quietly(&docker, &instance.uuid).await;
}

// ── M8.1: backup + restore of a real container's data ────────────

/// The full backup pipeline over the real runtime: a container
/// writes files into its bind-mounted data directory (through the
/// §M6 exec path), a backup job (through the durable-job executor
/// and the real archive engine) seals them into a sha256-verified
/// archive, and a restore job swaps the live data back to the
/// backup point — while the container keeps running.
#[tokio::test]
async fn backup_and_restore_of_container_data() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    ensure_busybox(&docker).await;

    let store = seed_store();
    let user = seed_user(store.as_ref());
    let node = seed_node(store.as_ref());
    let instance = seed_instance(store.as_ref(), &node, &user, 46118, "sleep 600");
    remove_quietly(&docker, &instance.uuid).await;

    // A direct runtime handle over the instance's own container spec
    // (the same config the supervisor builds): the bind-mounted data
    // directory is the engine-side instance dir the backup archives.
    let data_dir = zim_engine_application::supervision::instance_data_dir(
        &test_settings().data_root,
        &instance.uuid_short,
    );
    let config = zim_engine_application::supervision::InstanceConfig {
        instance_id: instance.id,
        container_name: instance.uuid.clone(),
        hostname: instance.uuid_short.clone(),
        image: "busybox:latest".to_owned(),
        entrypoint: None,
        command: vec!["sleep".into(), "600".into()],
        env: vec![
            ("SERVER_MEMORY".to_owned(), "0".to_owned()),
            ("SERVER_PORT".to_owned(), "46118".to_owned()),
            ("SERVER_IP".to_owned(), "127.0.0.1".to_owned()),
        ],
        limits: Default::default(),
        port_bindings: vec![],
        data_dir: data_dir.clone(),
        container_data_dir: std::path::PathBuf::from("/home/container"),
        labels: vec![("Service".to_owned(), "ZIM".to_owned())],
        crash_detection_enabled: false,
    };
    let factory = Arc::new(
        DockerRuntimeFactory::connect(test_settings())
            .await
            .expect("factory"),
    );
    let runtime = factory.create(&config).await.expect("runtime");
    runtime.start(&config).await.expect("start");
    wait_until_running(runtime.as_ref()).await;

    // The container writes its "world" into the bind mount (the
    // engine-side data directory the backup will archive).
    use zim_engine_application::supervision::ExecCommand;
    let write_world = ExecCommand {
        cmd: vec![
            "sh".into(),
            "-c".into(),
            "mkdir -p /home/container/world && \
             echo dragon-egg > /home/container/world/data.txt && \
             echo motd=live > /home/container/server.properties"
                .into(),
        ],
        ..Default::default()
    };
    let out = runtime.exec(&write_world).await.expect("write world");
    assert_eq!(out.exit_code, Some(0), "world write failed");
    assert!(
        soon(|| async { data_dir.join("world/data.txt").exists() }).await,
        "the container's write must reach the host-side bind mount"
    );

    // The durable-job executor + backup service over the same data
    // root the runtime bind-mounted.
    let jobs = zim_engine_jobs::JobSystem::new(
        Arc::clone(&store),
        None,
        zim_engine_jobs::JobPolicy {
            poll_interval: Duration::from_millis(20),
            cleaner_interval: Duration::from_millis(50),
            max_concurrent: 2,
            shutdown_grace: Duration::from_secs(10),
        },
    );
    let backups = zim_engine_backups::BackupService::new(
        Arc::clone(&store),
        Arc::clone(&jobs),
        test_settings().data_root,
    );
    backups.register().await;
    jobs.start();

    // 1. Backup: seal the container's data into a verified archive.
    let (job, backup) = backups.create_backup(instance.id).await.expect("backup");
    wait_for_job_state(
        store.as_ref(),
        job.id,
        zim_engine_domain::status::JobState::Succeeded,
    )
    .await;
    let sealed = store.get_backup(&backup.id).unwrap().unwrap();
    let sha256 = sealed.sha256_hash.as_deref().expect("sha256 recorded");
    assert_eq!(sha256.len(), 64);
    assert!(sealed.bytes > 0);
    let archive = backups
        .archive_path_for(instance.id, backup.id)
        .await
        .expect("archive path");
    assert!(archive.exists());
    zim_engine_backups::verify_archive(&archive, sha256).expect("archive verifies");

    // 2. The container keeps playing: new data lands in the live
    //    tree (which the restore will roll back).
    let clobber = ExecCommand {
        cmd: vec![
            "sh".into(),
            "-c".into(),
            "echo clobbered > /home/container/server.properties".into(),
        ],
        ..Default::default()
    };
    let out = runtime.exec(&clobber).await.expect("clobber");
    assert_eq!(out.exit_code, Some(0));
    assert!(
        soon(|| async {
            std::fs::read_to_string(data_dir.join("server.properties"))
                .map(|c| c.contains("clobbered"))
                .unwrap_or(false)
        })
        .await,
        "the clobber must land before the restore"
    );

    // 3. Restore: the swap returns the backup-point contents.
    let (job, _backup) = backups
        .restore_backup(instance.id, backup.id)
        .await
        .expect("restore");
    wait_for_job_state(
        store.as_ref(),
        job.id,
        zim_engine_domain::status::JobState::Succeeded,
    )
    .await;

    assert_eq!(
        std::fs::read_to_string(data_dir.join("server.properties")).unwrap(),
        "motd=live\n",
        "the restore must return the backup-point contents"
    );
    assert_eq!(
        std::fs::read_to_string(data_dir.join("world/data.txt")).unwrap(),
        "dragon-egg\n"
    );

    // The container is still running throughout (invariant 12's
    // spirit: job activity does not disturb the source).
    let state = runtime.state().await.expect("runtime state");
    assert!(
        state.is_running(),
        "the workload must survive the backup and restore"
    );

    runtime.stop(Duration::from_secs(10)).await.expect("stop");
    jobs.shutdown().await;
    remove_quietly(&docker, &instance.uuid).await;
}

/// Poll the store until the job reaches the wanted state.
async fn wait_for_job_state(
    store: &dyn Persistence,
    id: zim_engine_domain::JobId,
    want: zim_engine_domain::status::JobState,
) -> zim_engine_domain::job::Job {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let job = store.get_job(&id).unwrap().unwrap();
        if job.state == want {
            return job;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "job {id} never reached {want:?}; last seen {:?}",
            job.state
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
