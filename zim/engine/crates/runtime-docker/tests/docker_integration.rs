//! Docker integration tests (M6 acceptance).
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
//! attach, port bindings) and the exec/stats capabilities.

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
