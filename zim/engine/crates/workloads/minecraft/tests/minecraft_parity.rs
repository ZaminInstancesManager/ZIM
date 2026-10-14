//! Docker parity tests (M17 §M17 acceptance): a Minecraft-shaped
//! server container, driven through the workload handle's §4
//! capabilities.
//!
//! The container is the in-test-built **fakemc** image: busybox
//! plus a shell script that reproduces the Minecraft server's
//! observable console contract — the `Done (N.NNNs)! For help,
//! type "help"` startup marker, `list`'s player-count line, `say`'s
//! echo, and the `stop` convention (a save message, then a clean
//! exit 0). A real Java image would add hundreds of megabytes and
//! minutes to every CI run for no additional contract coverage;
//! the §M17 behaviors under test are the console conventions, not
//! the JVM.
//!
//! These self-skip when no Docker daemon answers (the main CI job
//! stays Docker-free; the `docker-integration` job runs them).

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use zim_engine_application::supervision::{
    InstanceConfig, ObservedState, PortBinding, Runtime, RuntimeFactory,
};
use zim_engine_workloads_minecraft::{GameServerSpec, MinecraftWorkload, StopOutcome};
use zim_engine_workloads_sdk::traits::{
    ConsoleCapable, ExecCapable, FilesystemCapable, MetricsCapable, Migratable,
};

/// The fakemc image tag (per-run unique so parallel runs never
/// collide).
const FAKEMC_BASE_TAG: &str = "zim-fakemc";

/// The fakemc script: the Minecraft console contract.
const FAKEMC_SCRIPT: &str = r#"#!/bin/sh
echo "Starting minecraft server version 1.20.4"
echo "Loading properties"
echo "Preparing level \"world\""
echo "Done (1.234s)! For help, type \"help\""
while IFS= read -r line; do
    case "$line" in
        stop)
            echo "Stopping the server"
            echo "Saving chunks for level 'ServerLevel[world]'"
            exit 0
            ;;
        list)
            echo "There are 0 of a max of 20 players online: "
            ;;
        say*)
            echo "[Server] ${line#say }"
            ;;
        *)
            echo "Unknown or incomplete command, see below for error"
            ;;
    esac
done
"#;

/// Unique container name per test run.
fn unique_name(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
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
fn test_settings() -> zim_engine_runtime_docker::DockerSettings {
    zim_engine_runtime_docker::DockerSettings {
        data_root: std::env::temp_dir().join("zim-mc-docker-test"),
        ..zim_engine_runtime_docker::DockerSettings::default()
    }
}

/// Build the fakemc image (busybox + the console-contract script),
/// returning its tag.
async fn ensure_fakemc(docker: &bollard::Docker) -> String {
    let tag = format!("{FAKEMC_BASE_TAG}:{}", uuid::Uuid::new_v4().simple());
    // The build context: a two-entry tar (Dockerfile + script).
    let mut context = tar::Builder::new(Vec::new());
    let dockerfile =
        "FROM busybox:latest\nCOPY fakemc.sh /fakemc.sh\nRUN chmod +x /fakemc.sh\nCMD [\"sh\", \"/fakemc.sh\"]\n";
    append_file(&mut context, "Dockerfile", dockerfile.as_bytes());
    append_file(&mut context, "fakemc.sh", FAKEMC_SCRIPT.as_bytes());
    let context_bytes = context.into_inner().expect("build context tar");

    // busybox must be present for FROM to resolve; pull when
    // missing (the same first-use pull the runtime performs).
    if docker.inspect_image("busybox:latest").await.is_err() {
        let options = bollard::query_parameters::CreateImageOptionsBuilder::new()
            .from_image("busybox")
            .tag("latest")
            .build();
        let mut stream = docker.create_image(Some(options), None, None);
        while let Some(item) = stream.next().await {
            item.expect("busybox pull progress");
        }
    }

    let options = bollard::query_parameters::BuildImageOptionsBuilder::default()
        .dockerfile("Dockerfile")
        .t(tag.as_str())
        .rm(true)
        .build();
    let mut stream = docker.build_image(
        options,
        None,
        Some(bollard::body_full(bytes::Bytes::from(context_bytes))),
    );
    while let Some(item) = stream.next().await {
        item.expect("fakemc image build progress");
    }
    tag
}

/// Append one file to an in-memory tar context.
fn append_file(builder: &mut tar::Builder<Vec<u8>>, name: &str, content: &[u8]) {
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    builder
        .append_data(&mut header, name, content)
        .expect("append context entry");
}

async fn remove_quietly(docker: &bollard::Docker, name: &str) {
    zim_engine_runtime_docker::remove_container_force(docker, name).await;
}

/// A fakemc instance config (the busybox config shape the runtime
/// suite uses, pointed at the fakemc image).
fn fakemc_config(name: &str, image: &str) -> (InstanceConfig, std::path::PathBuf) {
    let data_dir = std::env::temp_dir().join(format!("zim-mc-data-{}", name));
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let config = InstanceConfig {
        instance_id: zim_engine_domain::InstanceId::new(),
        container_name: name.to_owned(),
        hostname: name.to_owned(),
        image: image.to_owned(),
        entrypoint: None,
        command: vec!["sh".to_owned(), "/fakemc.sh".to_owned()],
        env: vec![
            ("SERVER_MEMORY".to_owned(), "512".to_owned()),
            ("SERVER_PORT".to_owned(), "25565".to_owned()),
            ("SERVER_IP".to_owned(), "127.0.0.1".to_owned()),
        ],
        limits: Default::default(),
        port_bindings: vec![PortBinding {
            host_ip: "127.0.0.1".parse().unwrap(),
            host_port: 25565,
            container_port: 25565,
            protocol: Default::default(),
        }],
        data_dir: data_dir.clone(),
        container_data_dir: std::path::PathBuf::from("/home/container"),
        labels: vec![("Service".to_owned(), "ZIM".to_owned())],
        crash_detection_enabled: true,
    };
    (config, data_dir)
}

/// A GameServerSpec shaped for fakemc (the Paper fixture's
/// behavioral bits with the image overridden).
fn fakemc_spec(image: &str) -> GameServerSpec {
    let report = zim_engine_eggs::import_legacy_json(
        r##"{
            "meta": { "version": "PTDL_v1", "update_url": null },
            "name": "Paper Minecraft",
            "description": "A high performance Minecraft server.",
            "author": "support@pterodactyl.io",
            "features": ["eula"],
            "docker_images": { "fakemc": "fakemc:local" },
            "startup": "java -Xms128M -Xmx{{SERVER_MEMORY}}M -jar {{SERVER_JARFILE}}",
            "config": {
                "files": "{}",
                "startup": { "done": "Done \\([0-9.]+s\\)! For help, type \\\"help\\\"" },
                "stop": { "command": "stop", "signal": false }
            },
            "variables": [
                {
                    "name": "Server Jar File",
                    "description": "The jar to run",
                    "env_variable": "SERVER_JARFILE",
                    "default_value": "server.jar",
                    "user_viewable": true,
                    "user_editable": true,
                    "rules": ["required"]
                }
            ]
        }"##,
        &zim_engine_eggs::ImportPolicy::default(),
    )
    .expect("import fixture egg");
    let mut spec = GameServerSpec::from_egg(&report.egg, Some("fakemc")).expect("spec");
    spec.image = image.to_owned();
    spec.image_label = Some("fakemc".to_owned());
    spec.eula_accepted = true;
    spec.validate().expect("fakemc spec validates");
    spec
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

/// Wait until the console tail contains a line matching the
/// startup-done regex.
async fn wait_for_startup_done(workload: &MinecraftWorkload) {
    let done_regex = regex::Regex::new(workload.spec().startup_done.as_deref().unwrap_or("Done"))
        .expect("startup_done regex");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let tail = workload.console_tail(64);
        if tail.iter().any(|line| done_regex.is_match(line)) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "startup-done line never appeared; console so far: {:?}",
            tail
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The §M17 acceptance core: install → start → console stream → the
/// stop convention.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_minecraft_console_and_stop_convention() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let image = ensure_fakemc(&docker).await;
    let name = unique_name("mc-conv");
    remove_quietly(&docker, &name).await;
    let (cfg, data_dir) = fakemc_config(&name, &image);
    let spec = fakemc_spec(&image);

    // The install plan's file steps, applied for real (eula.txt on
    // the bind-mounted data dir).
    let plan = zim_engine_workloads_minecraft::build_install_plan(&spec);
    let fs_api = zim_engine_files::Filesystem::new(&data_dir);
    zim_engine_workloads_minecraft::apply_file_steps(&plan, &fs_api).expect("apply install plan");
    assert_eq!(fs_api.read_file("eula.txt").unwrap(), b"eula=true\n");

    // The handle over the Docker runtime.
    let factory = zim_engine_runtime_docker::DockerRuntimeFactory::connect(test_settings())
        .await
        .expect("docker factory connects");
    let runtime = factory.create(&cfg).await.expect("runtime handle created");
    let workload = MinecraftWorkload::new(
        Arc::clone(&runtime),
        Arc::new(cfg.clone()),
        tokio::runtime::Handle::current(),
        spec,
    );

    // Start → Running, and the console stream shows real
    // Minecraft-shaped output (the startup-done marker).
    workload.start_async().await.expect("start");
    wait_until_running(runtime.as_ref()).await;
    workload.attach_console().await.expect("attach console");
    wait_for_startup_done(&workload).await;

    // Console input round-trips: `list` answers the player-count
    // line (the Minecraft convention the Panel's console UI rides).
    workload.send_input_async("list").await.expect("send list");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let tail = workload.console_tail(16);
        if tail.iter().any(|line| line.contains("players online")) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "`list` answer never appeared; console: {:?}",
            tail
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // The sync trait surface works from a blocking context
    // (spawn_blocking — the documented constraint).
    let blocking = workload.clone();
    let console_tail = tokio::task::spawn_blocking(move || blocking.console_tail(4))
        .await
        .expect("blocking join");
    assert!(!console_tail.is_empty());

    // Stop: the console line `stop`, the save messages, the clean
    // exit — the §M17 acceptance.
    let outcome = workload.stop_async().await.expect("stop");
    assert_eq!(
        outcome,
        StopOutcome::Console {
            exit_code: Some(0),
            oom_killed: false
        },
        "the fakemc server must exit 0 on the console stop convention"
    );
    let state = wait_until_offline(runtime.as_ref()).await;
    let ObservedState::Offline {
        exit_code: Some(0),
        oom_killed: false,
    } = state
    else {
        panic!("expected a clean exit 0, got {state:?}");
    };
    // The save messages are in the retained console window.
    let tail = workload.console_tail(8);
    assert!(tail.iter().any(|line| line.contains("Stopping the server")));
    assert!(tail.iter().any(|line| line.contains("Saving chunks")));

    remove_quietly(&docker, &name).await;
    let _ = data_dir;
}

/// The filesystem capability over the bind-mounted data directory,
/// archive/extract included.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_filesystem_capability_archives_and_extracts() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let image = ensure_fakemc(&docker).await;
    let name = unique_name("mc-files");
    remove_quietly(&docker, &name).await;
    let (cfg, data_dir) = fakemc_config(&name, &image);
    let spec = fakemc_spec(&image);

    let factory = zim_engine_runtime_docker::DockerRuntimeFactory::connect(test_settings())
        .await
        .expect("docker factory connects");
    let runtime = factory.create(&cfg).await.expect("runtime handle");
    let workload = MinecraftWorkload::new(
        Arc::clone(&runtime),
        Arc::new(cfg),
        tokio::runtime::Handle::current(),
        spec,
    );

    // Write through the capability (a real Minecraft-shaped tree),
    // read it back, list it.
    workload
        .write_file("server.properties", b"server-port=25565\nmotd=hi\n")
        .expect("write");
    workload
        .write_file("worlds/world/level.dat", b"leveldata")
        .expect("write nested");
    assert_eq!(
        workload.read_file("server.properties").unwrap(),
        b"server-port=25565\nmotd=hi\n"
    );
    let listing = workload.list_dir(".").expect("list");
    assert!(listing.contains(&"server.properties".to_owned()));
    assert!(listing.contains(&"worlds/".to_owned()));

    // The archive/extract pair (the Wings file API's compression
    // surface), through the sync trait from a blocking context.
    let blocking = workload.clone();
    let snapshot = tokio::task::spawn_blocking(move || {
        let reference = blocking
            .archive(
                "snapshots/pre-update.tar.gz",
                &["server.properties", "worlds"],
                zim_engine_workloads_sdk::traits::ArchiveFormat::TarGz,
            )
            .expect("archive");
        blocking
            .extract("snapshots/pre-update.tar.gz", "restored")
            .expect("extract");
        reference
    })
    .await
    .expect("blocking join");
    assert!(snapshot.bytes > 0);
    assert_eq!(
        workload
            .read_file("restored/worlds/world/level.dat")
            .unwrap(),
        b"leveldata"
    );
    assert_eq!(
        workload.read_file("restored/server.properties").unwrap(),
        b"server-port=25565\nmotd=hi\n"
    );

    // Delete works.
    workload.delete("restored").expect("delete");
    assert!(workload.read_file("restored/server.properties").is_err());

    remove_quietly(&docker, &name).await;
    let _ = data_dir;
}

/// Exec + metrics over a running fakemc.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exec_and_metrics_capabilities() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let image = ensure_fakemc(&docker).await;
    let name = unique_name("mc-exec");
    remove_quietly(&docker, &name).await;
    let (cfg, data_dir) = fakemc_config(&name, &image);
    let spec = fakemc_spec(&image);

    let factory = zim_engine_runtime_docker::DockerRuntimeFactory::connect(test_settings())
        .await
        .expect("docker factory connects");
    let runtime = factory.create(&cfg).await.expect("runtime handle");
    let workload = MinecraftWorkload::new(
        Arc::clone(&runtime),
        Arc::new(cfg),
        tokio::runtime::Handle::current(),
        spec,
    );

    workload.start_async().await.expect("start");
    wait_until_running(runtime.as_ref()).await;

    // Exec: a shell line in the workload's context (the SDK form).
    let blocking = workload.clone();
    let output = tokio::task::spawn_blocking(move || {
        blocking.exec(zim_engine_workloads_sdk::traits::ExecCommand {
            command: "echo hello-from-exec".to_owned(),
            timeout: Duration::from_secs(10),
        })
    })
    .await
    .expect("blocking join")
    .expect("exec");
    assert_eq!(output.exit_code, Some(0));
    assert!(output.stdout.contains("hello-from-exec"), "{output:?}");

    // Metrics: the Docker stats sample through the §4 shape.
    let blocking = workload.clone();
    let usage = tokio::task::spawn_blocking(move || blocking.metrics())
        .await
        .expect("blocking join")
        .expect("metrics");
    assert!(usage.memory_bytes > 0, "a running container reports memory");
    assert!(usage.cpu >= 0.0);

    let _ = workload.stop_async().await;
    remove_quietly(&docker, &name).await;
    let _ = data_dir;
}

/// The backup acceptance: create + restore over the workload's data
/// directory (the M8 archive engine, Minecraft-shaped data).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_backup_can_be_created_and_restored() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let image = ensure_fakemc(&docker).await;
    let name = unique_name("mc-backup");
    remove_quietly(&docker, &name).await;
    let (_cfg, data_dir) = fakemc_config(&name, &image);
    let spec = fakemc_spec(&image);

    // The installed workload's tree.
    let plan = zim_engine_workloads_minecraft::build_install_plan(&spec);
    let fs_api = zim_engine_files::Filesystem::new(&data_dir);
    zim_engine_workloads_minecraft::apply_file_steps(&plan, &fs_api).expect("apply install plan");
    fs_api
        .write_file("worlds/world/level.dat", b"the precious world")
        .expect("world data");

    // Create the backup (the M8 engine, driven the way the backup
    // job handler drives it).
    let backup_dir = std::env::temp_dir().join(format!("zim-mc-backup-{name}"));
    std::fs::create_dir_all(&backup_dir).expect("create backup dir");
    let archive_path = backup_dir.join("backup.tar.gz");
    let created = zim_engine_backups::create_archive(&data_dir, &archive_path, &|| false, &|_| {})
        .expect("create backup");
    assert!(created.bytes > 0);

    // Trash the live tree, restore the backup, verify the world
    // survived (restore swaps contents in place).
    fs_api.delete("worlds").expect("drop worlds");
    assert!(fs_api.read_file("eula.txt").is_ok());
    zim_engine_backups::restore_archive(
        &archive_path,
        &created.sha256,
        &data_dir,
        &|| false,
        &|_| {},
    )
    .expect("restore backup");
    assert_eq!(
        fs_api.read_file("worlds/world/level.dat").unwrap(),
        b"the precious world"
    );
    assert_eq!(fs_api.read_file("eula.txt").unwrap(), b"eula=true\n");

    remove_quietly(&docker, &name).await;
}

/// The migration family: quiesce (the stop convention), the
/// checkpoint, and the relaunch on finalize.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_migration_family_quiesces_and_relaunches() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let image = ensure_fakemc(&docker).await;
    let name = unique_name("mc-migrate");
    remove_quietly(&docker, &name).await;
    let (cfg, data_dir) = fakemc_config(&name, &image);
    let spec = fakemc_spec(&image);

    let factory = zim_engine_runtime_docker::DockerRuntimeFactory::connect(test_settings())
        .await
        .expect("docker factory connects");
    let runtime = factory.create(&cfg).await.expect("runtime handle");
    let workload = MinecraftWorkload::new(
        Arc::clone(&runtime),
        Arc::new(cfg),
        tokio::runtime::Handle::current(),
        spec,
    );

    workload.start_async().await.expect("start");
    wait_until_running(runtime.as_ref()).await;
    workload.attach_console().await.expect("attach");
    wait_for_startup_done(&workload).await;

    // The plan: the roadmap's §M17 contract — requires_quiesce.
    let blocking = workload.clone();
    let plan = tokio::task::spawn_blocking(move || blocking.migration_plan())
        .await
        .expect("blocking join")
        .expect("migration plan");
    assert!(plan.requires_quiesce);
    assert!(plan.estimated_downtime > Duration::ZERO);

    // Prepare: the quiesce stops the workload (the save runs first).
    let blocking = workload.clone();
    let checkpoint = tokio::task::spawn_blocking(move || blocking.prepare_for_migration())
        .await
        .expect("blocking join")
        .expect("prepare_for_migration");
    assert!(checkpoint.marker.starts_with("quiesced:"));
    wait_until_offline(runtime.as_ref()).await;

    // Finalize: the relaunch (the transfer "landed").
    let blocking = workload.clone();
    tokio::task::spawn_blocking(move || blocking.finalize_migration())
        .await
        .expect("blocking join")
        .expect("finalize_migration");
    wait_until_running(runtime.as_ref()).await;
    // The console contract holds after the relaunch.
    workload.attach_console().await.expect("re-attach");
    wait_for_startup_done(&workload).await;
    workload
        .send_input_async("say migrated")
        .await
        .expect("say");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let tail = workload.console_tail(8);
        if tail.iter().any(|line| line.contains("[Server] migrated")) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "`say` echo never appeared; console: {:?}",
            tail
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let _ = workload.stop_async().await;
    remove_quietly(&docker, &name).await;
    let _ = data_dir;
}
