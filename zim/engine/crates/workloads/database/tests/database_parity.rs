//! Docker parity tests (§M18 acceptance): real Postgres, MariaDB,
//! MySQL, and Redis containers, driven through the workload
//! handle's §4 capabilities.
//!
//! The suite runs the official images (not fixtures): the §M18
//! behaviors under test are the engines' own — `initdb` and WAL
//! replay, `mariadb-install-db` and InnoDB recovery, the mysqlx
//! second listener, the RDB lifecycle — and every one of them is
//! exercised through the engine's real client (`psql`,
//! `mariadb-admin`, `mysqladmin`, `redis-cli`) exactly the way the
//! handle's probes and eval paths do.
//!
//! The coverage map, per the roadmap's acceptance criteria:
//!
//! - **Postgres** — the full acceptance: install (first boot's
//!   `initdb`), start, query via the SQL client, snapshot
//!   (CHECKPOINT + archive), restore, and the readiness probe's
//!   `NotReady` window during initialization;
//! - **Postgres recovery** — a SIGKILL, the WAL replay on restart,
//!   the probe's `NotReady`-during-recovery window, and durability
//!   through it all;
//! - **Postgres migration** — the multi-step plan (marker,
//!   quiesce, snapshot, ship, restore, catch-up) to a second node,
//!   with the downtime measured;
//! - **MariaDB** — the MySQL family's lock-held snapshot
//!   (`FLUSH TABLES WITH READ LOCK` + `SYSTEM tar`) and its
//!   restore through InnoDB recovery;
//! - **MySQL** — the multi-endpoint network surface (the live
//!   mysqlx listener on 33060) and the expose/unexpose family;
//! - **Redis** — the `BGSAVE` snapshot, the restore, and the
//!   `LASTSAVE` migration marker.
//!
//! These self-skip when no Docker daemon answers (the main CI job
//! stays Docker-free; the `docker-integration` job runs them).

use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use bollard::Docker;
use futures_util::StreamExt;
use zim_engine_application::supervision::{
    InstanceConfig, ObservedState, PortBinding, PortProtocol, Runtime, RuntimeFactory,
};
use zim_engine_workloads_database::{
    DatabaseCredentials, DatabaseEngine, DatabaseSpec, DatabaseWorkload,
};
use zim_engine_workloads_sdk::traits::{
    Endpoint, MetricsCapable, Migratable, NetworkCapable, PortSpec, Readiness, ReadyProbeCapable,
};

/// Unique container name per test run.
fn unique_name(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

/// The next host port to bind (tests run in parallel; a fixed port
/// collides across containers — the bind failure is a 500 from the
/// daemon, not a test assertion).
fn unique_host_port() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static NEXT_PORT: AtomicU16 = AtomicU16::new(21111);
    NEXT_PORT.fetch_add(1, Ordering::Relaxed)
}

/// Connect to Docker or skip the test.
async fn docker_or_skip() -> Option<Docker> {
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

/// Test settings: the engine defaults, but a test data root.
fn test_settings() -> zim_engine_runtime_docker::DockerSettings {
    zim_engine_runtime_docker::DockerSettings {
        data_root: std::env::temp_dir().join("zim-db-docker-test"),
        ..zim_engine_runtime_docker::DockerSettings::default()
    }
}

/// Build the spec for one engine, test-credentialed.
fn test_spec(engine: DatabaseEngine) -> DatabaseSpec {
    DatabaseSpec {
        engine,
        image: None,
        version: None,
        credentials: DatabaseCredentials {
            username: None,
            password: "zim-test-pw".to_owned(),
            database: None,
            app_password: None,
        },
        endpoints: None,
        extra_args: Vec::new(),
    }
}

/// Build one engine's instance configuration from its spec: the
/// spec's resolved env/argv, the engine's container posture (the
/// M18 per-instance user + rootfs overrides), and one port binding
/// per distinct container port the endpoints declare.
fn db_config(spec: &DatabaseSpec, name: &str) -> (InstanceConfig, std::path::PathBuf) {
    let data_dir = std::env::temp_dir().join(format!("zim-db-data-{name}"));
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    // Dedupe the endpoints' container ports into bindings (the
    // read/write/replication logical names may share one listener).
    let mut bound: Vec<u16> = Vec::new();
    let mut port_bindings = Vec::new();
    for endpoint in spec.effective_endpoints() {
        if bound.contains(&endpoint.container_port) {
            continue;
        }
        bound.push(endpoint.container_port);
        port_bindings.push(PortBinding {
            host_ip: "127.0.0.1".parse::<IpAddr>().unwrap(),
            host_port: unique_host_port(),
            container_port: endpoint.container_port,
            protocol: PortProtocol::Tcp,
        });
    }

    let posture = spec.container_posture();
    let config = InstanceConfig {
        instance_id: zim_engine_domain::InstanceId::new(),
        container_name: name.to_owned(),
        hostname: name.to_owned(),
        image: spec.effective_image().to_owned(),
        entrypoint: None,
        command: spec.resolved_command(),
        env: spec.resolved_env(),
        limits: Default::default(),
        port_bindings,
        data_dir: data_dir.clone(),
        container_data_dir: std::path::PathBuf::from(spec.engine.default_container_data_dir()),
        labels: vec![("Service".to_owned(), "ZIM".to_owned())],
        crash_detection_enabled: true,
        user: Some(posture.user),
        readonly_rootfs: Some(posture.readonly_rootfs),
        cap_drop: Some(posture.cap_drop),
    };
    (config, data_dir)
}

/// Place one workload: connect the factory, create the runtime
/// handle, and build the workload handle.
async fn place(
    spec: DatabaseSpec,
    name: &str,
) -> (
    DatabaseWorkload,
    Arc<dyn Runtime>,
    Docker,
    String,
    std::path::PathBuf,
) {
    let docker = bollard::Docker::connect_with_local_defaults().expect("docker client");
    let (config, data_dir) = db_config(&spec, name);
    let factory = zim_engine_runtime_docker::DockerRuntimeFactory::connect(test_settings())
        .await
        .expect("docker factory connects");
    let runtime = factory
        .create(&config)
        .await
        .expect("runtime handle created");
    let workload = DatabaseWorkload::new(
        Arc::clone(&runtime),
        Arc::new(config),
        tokio::runtime::Handle::current(),
        spec,
    );
    (workload, runtime, docker, name.to_owned(), data_dir)
}

async fn remove_quietly(docker: &Docker, name: &str) {
    zim_engine_runtime_docker::remove_container_force(docker, name).await;
}

async fn wait_until_running(runtime: &dyn Runtime) -> ObservedState {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
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

/// One probe, now (the async twin — tests run in async context).
async fn probe_now(workload: &DatabaseWorkload) -> Readiness {
    workload.probe_async().await.expect("probe runs")
}

/// Evaluate one statement through the engine's own client and
/// require a clean exit.
async fn eval_ok(workload: &DatabaseWorkload, statement: &str) -> String {
    let output = workload
        .eval_async(statement)
        .await
        .expect("the engine client runs");
    assert_eq!(
        output.exit_code,
        Some(0),
        "statement {statement:?} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The suite's serialization gate: the four engines' first boots
/// (initdb, mariadb-install-db, mysqld --initialize) are heavy
/// enough that running them concurrently starves a shared runner's
/// CPU and turns the readiness windows into deadline flakes — the
/// suite runs one container at a time, deterministically.
static SUITE_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The container's log tail (the failure diagnostics' payload).
async fn docker_log_tail(docker: &Docker, name: &str) -> String {
    use futures_util::StreamExt;
    let options = bollard::query_parameters::LogsOptionsBuilder::new()
        .stdout(true)
        .stderr(true)
        .tail("120")
        .timestamps(false)
        .build();
    let mut stream = docker.logs(name, Some(options));
    let mut out = String::new();
    while let Some(chunk) = stream.next().await {
        let Ok(log) = chunk else { break };
        let message: &[u8] = match &log {
            bollard::container::LogOutput::StdOut { message } => message,
            bollard::container::LogOutput::StdErr { message } => message,
            other => other.as_ref(),
        };
        out.push_str(&String::from_utf8_lossy(message));
    }
    out
}

/// Wait for readiness, recording the probe reasons; on failure,
/// dump the container's log tail (the engine's own startup
/// diagnostics — the entrypoint's errors, the recovery's pace)
/// before the panic, so a CI failure is diagnosable from the log
/// alone.
async fn wait_ready_logged(
    workload: &DatabaseWorkload,
    docker: &Docker,
    name: &str,
    deadline: Duration,
) {
    let started = tokio::time::Instant::now();
    let mut reasons: Vec<String> = Vec::new();
    loop {
        match workload.probe_async().await.expect("probe runs") {
            Readiness::Ready => return,
            other => {
                let reason = match &other {
                    Readiness::NotReady { reason } => reason.clone(),
                    Readiness::Unknown => "Unknown".to_owned(),
                    Readiness::Ready => unreachable!(),
                };
                if reasons.last().map(String::as_str) != Some(reason.as_str()) {
                    reasons.push(reason);
                }
            }
        }
        if started.elapsed() >= deadline {
            let logs = docker_log_tail(docker, name).await;
            panic!(
                "the workload did not become ready within {deadline:?}; \
                 probe reasons seen: {reasons:?}\n--- container logs ---\n{logs}"
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The container's published container-port keys (the docker
/// inspect view of the port surface).
async fn published_ports(docker: &Docker, name: &str) -> Vec<String> {
    let inspect = docker
        .inspect_container(
            name,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .expect("inspect the container");
    inspect
        .host_config
        .and_then(|host| host.port_bindings)
        .map(|bindings| bindings.keys().cloned().collect())
        .unwrap_or_default()
}

/// Evaluate one statement through the engine's own client,
/// retrying connection-shaped failures.
///
/// The database images' first boots *flap* readiness: Postgres's
/// initialization runs an accepting temp server, stops it, then
/// starts the real one — a caller that observed Ready can race the
/// gap. Connection-shaped failures (psql's "could not connect",
/// the clients' "Connection refused" / "starting up") are retried
/// for the window; everything else fails immediately.
async fn eval_ok_retrying(workload: &DatabaseWorkload, statement: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        let output = workload
            .eval_async(statement)
            .await
            .expect("the engine client runs");
        if output.exit_code == Some(0) {
            return String::from_utf8_lossy(&output.stdout).into_owned();
        }
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let connection_shaped = stderr.contains("could not connect")
            || stderr.contains("Connection refused")
            || stderr.contains("Can't connect")
            || stdout.contains("Connection refused");
        if !connection_shaped || tokio::time::Instant::now() >= deadline {
            panic!("statement {statement:?} failed: {stdout}{stderr}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The §M18 acceptance, Postgres: install → start → query →
/// snapshot → restore, with the readiness probe's NotReady window
/// observed during first-boot initialization.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_postgres_acceptance_install_start_query_snapshot_restore() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let mut spec = test_spec(DatabaseEngine::Postgres);
    spec.credentials.database = Some("app".to_owned());
    let name = unique_name("pg-acc");
    remove_quietly(&docker, &name).await;
    let (workload, runtime, docker, name, data_dir) = place(spec, &name).await;
    let _serial = SUITE_GATE.lock().await;

    // Install + start: the first boot runs the image's own
    // installer (`initdb` — seconds of it), which the readiness
    // probe's NotReady window is.
    workload.start_async().await.expect("start");
    wait_until_running(runtime.as_ref()).await;
    let initial = probe_now(&workload).await;
    assert!(
        matches!(initial, Readiness::NotReady { .. }),
        "the probe must report NotReady during initialization, got {initial:?}"
    );
    wait_ready_logged(&workload, &docker, &name, Duration::from_secs(180)).await;

    // Query via the SQL client (psql over the container's socket,
    // the engine's own client — the acceptance's "queried via a
    // SQL client").
    eval_ok_retrying(
        &workload,
        "CREATE TABLE zim_acceptance (id serial PRIMARY KEY, note text NOT NULL)",
    )
    .await;
    eval_ok_retrying(
        &workload,
        "INSERT INTO zim_acceptance (note) VALUES ('first'), ('second')",
    )
    .await;
    let count = eval_ok(&workload, "SELECT count(*) FROM zim_acceptance").await;
    assert_eq!(count.trim(), "2", "the count query's tuple output");

    // Snapshot: the CHECKPOINT quiesce + the host-side archive.
    let snapshot = workload
        .snapshot_async("pre-migrate")
        .await
        .expect("the CHECKPOINT snapshot");
    assert_eq!(snapshot.label, "pre-migrate");

    // Mutate past the snapshot point.
    eval_ok(&workload, "DELETE FROM zim_acceptance").await;
    eval_ok(
        &workload,
        "INSERT INTO zim_acceptance (note) VALUES ('post-snapshot')",
    )
    .await;
    let count = eval_ok(&workload, "SELECT count(*) FROM zim_acceptance").await;
    assert_eq!(count.trim(), "1");

    // Restore: stop → clear → extract → start → ready (the WAL
    // replay completes the recovery).
    workload
        .restore_snapshot_async(&snapshot)
        .await
        .expect("the restore");
    let count = eval_ok(&workload, "SELECT count(*) FROM zim_acceptance").await;
    assert_eq!(
        count.trim(),
        "2",
        "the restored workload serves the snapshot's state, not the mutations"
    );
    let notes = eval_ok(&workload, "SELECT note FROM zim_acceptance ORDER BY id").await;
    assert!(
        notes.contains("first") && notes.contains("second"),
        "{notes}"
    );

    // The sync trait surface works from a blocking context
    // (spawn_blocking — the documented constraint).
    let blocking = workload.clone();
    let readiness = tokio::task::spawn_blocking(move || blocking.readiness())
        .await
        .expect("blocking join")
        .expect("readiness");
    assert_eq!(readiness, Readiness::Ready);

    let _ = workload.stop_async().await;
    remove_quietly(&docker, &name).await;
    let _ = (docker, name, data_dir);
}

/// The §M18 recovery acceptance, Postgres: a SIGKILL, the WAL
/// replay on restart, the probe's NotReady window during the
/// recovery, and durability through all of it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_postgres_recovery_readiness_and_durability() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let spec = test_spec(DatabaseEngine::Postgres);
    let name = unique_name("pg-rec");
    remove_quietly(&docker, &name).await;
    let (workload, runtime, docker, name, data_dir) = place(spec, &name).await;
    let _serial = SUITE_GATE.lock().await;

    workload.start_async().await.expect("start");
    wait_until_running(runtime.as_ref()).await;
    wait_ready_logged(&workload, &docker, &name, Duration::from_secs(180)).await;

    // Enough committed data that the kill leaves real WAL to
    // replay (a durable window the recovery poll can observe).
    eval_ok_retrying(
        &workload,
        "CREATE TABLE zim_recovery (id int PRIMARY KEY, payload text)",
    )
    .await;
    eval_ok_retrying(
        &workload,
        "INSERT INTO zim_recovery SELECT g, repeat('x', 32) FROM generate_series(1, 50000) g",
    )
    .await;

    // The unclean kill (SIGKILL — no shutdown, no checkpoint).
    docker
        .kill_container(
            &name,
            None::<bollard::query_parameters::KillContainerOptions>,
        )
        .await
        .expect("the kill");
    wait_until_offline(runtime.as_ref()).await;

    // Restart: the recovery window. Poll as fast as the probe
    // round-trip allows and REQUIRE at least one NotReady
    // observation before the Ready (the §M18 acceptance's exact
    // demand — the probe correctly reports NotReady during
    // recovery and Ready after).
    workload.start_async().await.expect("restart");
    wait_until_running(runtime.as_ref()).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    let mut saw_not_ready = false;
    let readiness = loop {
        let readiness = probe_now(&workload).await;
        if matches!(readiness, Readiness::NotReady { .. }) {
            saw_not_ready = true;
            eprintln!("zim: recovery-window probe: {readiness:?}");
        }
        if readiness == Readiness::Ready {
            break readiness;
        }
        if tokio::time::Instant::now() >= deadline {
            let logs = docker_log_tail(&docker, &name).await;
            panic!(
                "the workload never became ready after the kill; \
                 last probe: {readiness:?}\n--- container logs ---\n{logs}"
            );
        }
    };
    assert_eq!(readiness, Readiness::Ready);
    assert!(
        saw_not_ready,
        "the probe must report NotReady at least once during the WAL replay"
    );

    // Durability: the committed rows survived the SIGKILL and the
    // replay.
    let count = eval_ok(&workload, "SELECT count(*) FROM zim_recovery").await;
    assert_eq!(count.trim(), "50000");

    let _ = workload.stop_async().await;
    remove_quietly(&docker, &name).await;
    let _ = (docker, name, data_dir);
}

/// The §M18 migration acceptance, Postgres: the multi-step plan
/// (marker, quiesce, snapshot, ship, restore, catch-up) to a second
/// node, with the downtime measured.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_postgres_migration_to_a_second_node() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let spec = test_spec(DatabaseEngine::Postgres);
    let node_a = unique_name("pg-mig-a");
    let node_b = unique_name("pg-mig-b");
    remove_quietly(&docker, &node_a).await;
    remove_quietly(&docker, &node_b).await;
    let _serial = SUITE_GATE.lock().await;

    let (source, runtime_a, _docker_a, node_a, data_a) = place(spec.clone(), &node_a).await;
    source.start_async().await.expect("node A start");
    wait_until_running(runtime_a.as_ref()).await;
    wait_ready_logged(&source, &docker, &node_a, Duration::from_secs(180)).await;

    eval_ok_retrying(&source, "CREATE TABLE zim_migration (note text NOT NULL)").await;
    eval_ok_retrying(
        &source,
        "INSERT INTO zim_migration (note) VALUES ('migrated-row')",
    )
    .await;

    // The multi-step plan, documented and asserted.
    let steps = zim_engine_workloads_database::migration_steps(&spec);
    assert_eq!(
        steps
            .iter()
            .map(|step| step.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "quiesce",
            "checkpoint",
            "snapshot",
            "ship",
            "restore",
            "catch-up"
        ]
    );

    // The plan + the quiesce contract.
    let blocking = source.clone();
    let plan = tokio::task::spawn_blocking(move || blocking.migration_plan())
        .await
        .expect("blocking join")
        .expect("migration plan");
    assert!(plan.requires_quiesce);
    assert!(plan.estimated_downtime > Duration::ZERO);

    // Steps 1-2: quiesce + checkpoint — the marker (the WAL LSN)
    // and the orderly shutdown. The downtime clock starts here.
    let downtime_started = tokio::time::Instant::now();
    let blocking = source.clone();
    let checkpoint = tokio::task::spawn_blocking(move || blocking.prepare_for_migration())
        .await
        .expect("blocking join")
        .expect("prepare_for_migration");
    assert!(
        checkpoint.marker.starts_with("postgres:0/"),
        "the marker carries the WAL LSN: {}",
        checkpoint.marker
    );
    assert!(checkpoint.marker.contains("quiesced="));
    wait_until_offline(runtime_a.as_ref()).await;

    // Step 3: snapshot (the quiesced engine archives directly).
    let snapshot = source
        .snapshot_async("migration-ship")
        .await
        .expect("the migration snapshot");

    // Step 4: ship — the archive transfers to node B's snapshot
    // store through the daemon's own file-transfer surface (the
    // `docker cp` API): download the archive from node A (the
    // daemon reads the ownership-taken data directory), then
    // unpack it into node B's still-runner-owned data directory.
    // This is the transfer engine's file-copy family, stood in by
    // the daemon.
    let (dest, runtime_b, docker, node_b, data_b) = place(spec, &node_b).await;
    let download = bollard::query_parameters::DownloadFromContainerOptionsBuilder::new()
        .path(&format!(
            "{}/snapshots/migration-ship.tar.gz",
            "/var/lib/postgresql/data"
        ))
        .build();
    let mut stream = docker.download_from_container(&node_a, Some(download));
    let mut blob = Vec::new();
    while let Some(chunk) = stream.next().await {
        blob.extend_from_slice(&chunk.expect("the archive download"));
    }
    assert!(!blob.is_empty(), "the archive downloaded");
    let ship_to = data_b.join("snapshots");
    std::fs::create_dir_all(&ship_to).expect("node B snapshot store");
    {
        // The blob is the docker-archive tar wrapping the file;
        // unpack it into node B's snapshot store host-side (the
        // directory is still runner-owned before B's first start).
        let cursor = std::io::Cursor::new(&blob);
        let mut archive = tar::Archive::new(cursor);
        archive.unpack(&ship_to).expect("the snapshot ships");
    }
    assert!(
        ship_to.join("migration-ship.tar.gz").is_file(),
        "the archive landed"
    );

    // Steps 5-6: restore + catch-up — the extract, the relaunch,
    // the WAL replay to the marker's LSN. The downtime clock ends
    // at readiness.
    dest.restore_snapshot_async(&snapshot)
        .await
        .expect("node B restores the shipped snapshot");
    let downtime = downtime_started.elapsed();

    // The catch-up: node B serves the pre-migration data.
    let count = eval_ok(&dest, "SELECT count(*) FROM zim_migration").await;
    assert_eq!(count.trim(), "1");
    let note = eval_ok(&dest, "SELECT note FROM zim_migration").await;
    assert_eq!(note.trim(), "migrated-row");

    // Node B accepts new writes (the migration's end state).
    eval_ok(
        &dest,
        "INSERT INTO zim_migration (note) VALUES ('node-b-write')",
    )
    .await;
    let count = eval_ok(&dest, "SELECT count(*) FROM zim_migration").await;
    assert_eq!(count.trim(), "2");

    // The documented downtime (§M18's "documented downtime").
    eprintln!(
        "zim: the documented migration downtime: {:?} (marker {})",
        downtime, checkpoint.marker
    );
    assert!(downtime > Duration::ZERO);
    assert!(
        downtime < Duration::from_secs(240),
        "the migration took {downtime:?}, far past the estimate"
    );

    // Finalize on node B (the relaunch contract) and abort on
    // node A (the local bytes were never disturbed — the
    // relaunch).
    let blocking = dest.clone();
    tokio::task::spawn_blocking(move || blocking.finalize_migration())
        .await
        .expect("blocking join")
        .expect("finalize_migration");
    wait_ready_logged(&dest, &docker, &node_b, Duration::from_secs(180)).await;

    let abort_checkpoint = zim_engine_workloads_sdk::traits::MigrationCheckpoint {
        marker: checkpoint.marker.clone(),
    };
    let blocking = source.clone();
    tokio::task::spawn_blocking(move || blocking.abort_migration(abort_checkpoint))
        .await
        .expect("blocking join")
        .expect("abort_migration");
    wait_until_running(runtime_a.as_ref()).await;

    let _ = source.stop_async().await;
    let _ = dest.stop_async().await;
    remove_quietly(&docker, &node_a).await;
    remove_quietly(&docker, &node_b).await;
    let _ = (docker, data_a, data_b, runtime_b);
}

/// The MySQL family's snapshot acceptance, MariaDB: the
/// lock-held archive (`FLUSH TABLES WITH READ LOCK` + the
/// session's `SYSTEM tar`) and its restore through InnoDB
/// recovery.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_mariadb_snapshot_under_flush_tables_with_read_lock() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let spec = test_spec(DatabaseEngine::MariaDb);
    let name = unique_name("mdb-snap");
    remove_quietly(&docker, &name).await;
    let (workload, runtime, docker, name, data_dir) = place(spec, &name).await;
    let _serial = SUITE_GATE.lock().await;
    let workload = workload.with_restore_ready_timeout(Duration::from_secs(300));

    // First boot: the image's initializer (`mariadb-install-db`)
    // is the readiness window.
    workload.start_async().await.expect("start");
    wait_until_running(runtime.as_ref()).await;
    let initial = probe_now(&workload).await;
    assert!(
        matches!(initial, Readiness::NotReady { .. }),
        "the probe must report NotReady during the bootstrap, got {initial:?}"
    );
    wait_ready_logged(&workload, &docker, &name, Duration::from_secs(300)).await;

    // The workload's data.
    eval_ok_retrying(&workload, "CREATE DATABASE IF NOT EXISTS zim").await;
    eval_ok_retrying(
        &workload,
        "CREATE TABLE zim.locked (id int PRIMARY KEY, note text NOT NULL)",
    )
    .await;
    eval_ok(
        &workload,
        "INSERT INTO zim.locked VALUES (1, 'first'), (2, 'second')",
    )
    .await;

    // The lock-held snapshot.
    let snapshot = workload
        .snapshot_async("locked")
        .await
        .expect("the FLUSH TABLES WITH READ LOCK snapshot");
    assert_eq!(snapshot.label, "locked");

    // Mutate past the snapshot point.
    eval_ok(&workload, "DELETE FROM zim.locked").await;
    eval_ok(
        &workload,
        "INSERT INTO zim.locked VALUES (3, 'post-snapshot')",
    )
    .await;
    let rows = eval_ok(&workload, "SELECT note FROM zim.locked").await;
    assert!(rows.contains("post-snapshot"));

    // Restore through InnoDB recovery.
    workload
        .restore_snapshot_async(&snapshot)
        .await
        .expect("the mariadb restore");
    let rows = eval_ok(&workload, "SELECT note FROM zim.locked ORDER BY id").await;
    assert!(
        rows.contains("first") && rows.contains("second"),
        "the restored rows: {rows}"
    );
    assert!(!rows.contains("post-snapshot"), "the rows: {rows}");

    let _ = workload.stop_async().await;
    remove_quietly(&docker, &name).await;
    let _ = (docker, name, data_dir);
}

/// The multi-endpoint network acceptance, MySQL: the live
/// listeners (3306 primary + 33060 mysqlx), the expose/unexpose
/// family, and the readiness probe's bootstrap window.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_mysql_endpoints_readiness_and_expose() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let spec = test_spec(DatabaseEngine::MySql);
    let name = unique_name("my-net");
    remove_quietly(&docker, &name).await;
    let (workload, runtime, docker, name, data_dir) = place(spec, &name).await;
    let _serial = SUITE_GATE.lock().await;

    workload.start_async().await.expect("start");
    wait_until_running(runtime.as_ref()).await;
    let initial = probe_now(&workload).await;
    assert!(
        matches!(initial, Readiness::NotReady { .. }),
        "the probe must report NotReady during the bootstrap, got {initial:?}"
    );
    wait_ready_logged(&workload, &docker, &name, Duration::from_secs(420)).await;

    // The declared endpoints: primary (3306) and mysqlx (33060),
    // each resolved to its placed host port.
    let endpoints = workload.endpoints().expect("endpoints");
    let by_name: Vec<&Endpoint> = endpoints
        .iter()
        .filter(|endpoint| endpoint.name == "primary")
        .collect();
    assert_eq!(by_name.len(), 1);
    let primary = by_name[0];
    let mysqlx = endpoints
        .iter()
        .find(|endpoint| endpoint.name == "mysqlx")
        .expect("the mysqlx endpoint");
    assert_ne!(primary.port, mysqlx.port);

    // Both listeners are live: a plain TCP connect lands on each.
    for endpoint in [primary, mysqlx] {
        let addr = SocketAddr::new("127.0.0.1".parse::<IpAddr>().unwrap(), endpoint.port);
        TcpStream::connect_timeout(&addr, Duration::from_secs(5))
            .unwrap_or_else(|e| panic!("connect {addr}: {e}"));
    }

    // The expose family: a new endpoint binds at the next start
    // (Docker cannot re-plumb a live container).
    let blocking = workload.clone();
    let exposed = tokio::task::spawn_blocking(move || {
        blocking.expose_port(PortSpec {
            name: "metrics".to_owned(),
            container_port: 3307,
            protocol: "tcp",
        })
    })
    .await
    .expect("blocking join")
    .expect("expose_port");
    assert_eq!(exposed.name, "metrics");
    workload.start_async().await.expect("restart applies");
    wait_until_running(runtime.as_ref()).await;
    let bindings = published_ports(&docker, &name).await;
    assert!(
        bindings.contains(&"3307/tcp".to_owned()),
        "the exposed port is published: {bindings:?}"
    );

    // And unexpose removes it.
    let blocking = workload.clone();
    tokio::task::spawn_blocking(move || blocking.unexpose_port(&exposed))
        .await
        .expect("blocking join")
        .expect("unexpose_port");
    workload.start_async().await.expect("restart applies");
    wait_until_running(runtime.as_ref()).await;
    let bindings = published_ports(&docker, &name).await;
    assert!(
        !bindings.contains(&"3307/tcp".to_owned()),
        "the unexposed port is gone: {bindings:?}"
    );
    assert!(bindings.contains(&"3306/tcp".to_owned()));
    assert!(bindings.contains(&"33060/tcp".to_owned()));

    let _ = workload.stop_async().await;
    remove_quietly(&docker, &name).await;
    let _ = (docker, name, data_dir);
}

/// The Redis acceptance: the BGSAVE snapshot, the restore, the
/// LASTSAVE migration marker, and the metrics sample.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_redis_snapshot_restore_and_marker() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let spec = test_spec(DatabaseEngine::Redis);
    let name = unique_name("redis-snap");
    remove_quietly(&docker, &name).await;
    let (workload, runtime, docker, name, data_dir) = place(spec, &name).await;
    let _serial = SUITE_GATE.lock().await;

    workload.start_async().await.expect("start");
    wait_until_running(runtime.as_ref()).await;
    wait_ready_logged(&workload, &docker, &name, Duration::from_secs(60)).await;

    eval_ok_retrying(&workload, "SET zim:key snapshot-era").await;
    let value = eval_ok(&workload, "GET zim:key").await;
    assert_eq!(value.trim(), "snapshot-era");

    // The BGSAVE snapshot.
    let snapshot = workload
        .snapshot_async("pre")
        .await
        .expect("the BGSAVE snapshot");

    // Mutate past the snapshot point.
    eval_ok(&workload, "SET zim:key post-snapshot").await;
    eval_ok(&workload, "SET zim:extra late-key").await;

    // Restore: the RDB's keyspace comes back.
    workload
        .restore_snapshot_async(&snapshot)
        .await
        .expect("the redis restore");
    let value = eval_ok(&workload, "GET zim:key").await;
    assert_eq!(value.trim(), "snapshot-era");
    let extra = eval_ok(&workload, "EXISTS zim:extra").await;
    assert_eq!(extra.trim(), "0", "the post-snapshot key is gone");

    // The migration marker (LASTSAVE) and the quiesce family.
    let blocking = workload.clone();
    let checkpoint = tokio::task::spawn_blocking(move || blocking.prepare_for_migration())
        .await
        .expect("blocking join")
        .expect("prepare_for_migration");
    let lastsave = checkpoint
        .marker
        .strip_prefix("redis:")
        .and_then(|rest| rest.split(':').next())
        .and_then(|epoch| epoch.parse::<u64>().ok())
        .expect("the LASTSAVE epoch in the marker");
    assert!(lastsave > 0, "{}", checkpoint.marker);
    wait_until_offline(runtime.as_ref()).await;

    // Finalize: the relaunch (the destination-side contract).
    let blocking = workload.clone();
    tokio::task::spawn_blocking(move || blocking.finalize_migration())
        .await
        .expect("blocking join")
        .expect("finalize_migration");
    wait_ready_logged(&workload, &docker, &name, Duration::from_secs(60)).await;
    let value = eval_ok(&workload, "GET zim:key").await;
    assert_eq!(value.trim(), "snapshot-era");

    // The metrics sample through the §4 shape.
    let blocking = workload.clone();
    let usage = tokio::task::spawn_blocking(move || blocking.metrics())
        .await
        .expect("blocking join")
        .expect("metrics");
    assert!(usage.memory_bytes > 0, "a running container reports memory");

    let _ = workload.stop_async().await;
    remove_quietly(&docker, &name).await;
    let _ = (docker, name, data_dir);
}
