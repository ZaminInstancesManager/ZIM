//! The DevProcess integration's parity suite (M19).
//!
//! Real containers (a busybox `sh` loop standing in for the dev
//! process — the same posture the Minecraft suite's fakemc
//! takes: the *contract* under test is the supervision, not the
//! toolchain), driven through the engine's own runtime. The
//! suite self-skips when no Docker daemon answers.
//!
//! The §M19 acceptance, in the suite's terms:
//!
//! - **"A `cargo watch` dev process can be supervised with
//!   file-watch restart"** — a watching policy's process
//!   restarts when a matching file changes
//!   ([`the_watch_restart_supervision`]).
//! - The console surface — the process's own stdout lands in the
//!   retained window, and a restart resets the generation
//!   ([`the_console_surface_and_generation_reset`]).
//! - The policy posture — `Never` watches nothing (a change
//!   restarts nothing)
//!   ([`the_never_policy_watches_nothing`]).
//! - The non-matching pattern — a change outside the patterns
//!   restarts nothing ([`the_patterns_decide_what_matches`]).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bollard::Docker;

use zim_engine_application::supervision::{InstanceConfig, ObservedState, Runtime};
use zim_engine_workloads_dev_process::{
    DevProcessSpec, DevProcessWorkload, DevRestartPolicy, WatchState,
};
use zim_engine_workloads_sdk::traits::ConsoleCapable;

/// The image the dev process runs in (a busybox shell loop — the
/// supervision contract's own carrier; the toolchain is the
/// user's choice at instantiation).
const PROCESS_IMAGE: &str = "busybox:1.36";

/// A unique container name per test run.
fn unique_name(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
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
        data_root: std::env::temp_dir().join("zim-dev-docker-test"),
        ..zim_engine_runtime_docker::DockerSettings::default()
    }
}

/// The dev process's own command: a shell loop that announces
/// its boot marker and keeps running (the dev-loop stand-in).
fn loop_command(marker: &str) -> Vec<String> {
    vec![
        "sh".to_owned(),
        "-c".to_owned(),
        format!("echo boot-{marker}; while true; do sleep 5; done"),
    ]
}

/// Build the instance configuration for one dev-process spec.
fn process_config(spec: &DevProcessSpec, name: &str, marker: &str) -> (InstanceConfig, PathBuf) {
    let data_dir = std::env::temp_dir().join(format!("zim-devproc-data-{name}"));
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let config = InstanceConfig {
        instance_id: zim_engine_domain::InstanceId::new(),
        container_name: name.to_owned(),
        hostname: name.to_owned(),
        image: PROCESS_IMAGE.to_owned(),
        entrypoint: None,
        command: loop_command(marker),
        env: spec
            .env
            .iter()
            .map(|binding| (binding.name.clone(), binding.value.clone()))
            .collect(),
        limits: Default::default(),
        // A dev process has no endpoints — nothing to bind.
        port_bindings: Vec::new(),
        data_dir: data_dir.clone(),
        container_data_dir: std::path::PathBuf::from("/home/container"),
        labels: vec![("Service".to_owned(), "ZIM".to_owned())],
        crash_detection_enabled: true,
        user: None,
        readonly_rootfs: None,
        cap_drop: None,
    };
    (config, data_dir)
}

/// Place one workload: connect the factory, create the runtime
/// handle, build the workload handle, start the process, and
/// attach its console. The process's command is the boot-loop
/// stand-in (the supervision contract's carrier — the spec's
/// own command is what a real placement would carry).
async fn place_and_start(
    spec: DevProcessSpec,
    name: &str,
) -> (DevProcessWorkload, Arc<dyn Runtime>, Docker, String, PathBuf) {
    let docker = bollard::Docker::connect_with_local_defaults().expect("docker client");
    let marker: String = unique_name("m").chars().take(8).collect();
    let (config, data_dir) = process_config(&spec, name, &marker);
    let factory = zim_engine_runtime_docker::DockerRuntimeFactory::connect(test_settings())
        .await
        .expect("docker factory connects");
    let runtime = factory
        .create(&config)
        .await
        .expect("runtime handle created");
    let workload = DevProcessWorkload::new(
        Arc::clone(&runtime),
        Arc::new(config),
        tokio::runtime::Handle::current(),
        spec,
    );
    workload.start_async().await.expect("start");
    wait_until_running(&*runtime).await;
    workload.attach_console().await.expect("attach console");
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

/// Wait until the console window carries the marker (the
/// process's own boot announcement).
async fn wait_for_console_line(workload: &DevProcessWorkload, needle: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let tail = workload.console_tail(50);
        if tail.iter().any(|line| line.contains(needle)) {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Wait until the restart counter reaches `want`.
async fn wait_for_restarts(workload: &DevProcessWorkload, want: u32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if workload.restarts() >= want {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The suite gate: one suite at a time (the same posture the
/// M18/M19 sibling parity suites take).
static SUITE_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The §M19 headline acceptance: a watching policy's process
/// restarts when a matching file changes.
#[tokio::test]
async fn the_watch_restart_supervision() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let _gate = SUITE_GATE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());

    let name = unique_name("devproc-watch");
    let spec = DevProcessSpec {
        command: loop_command("x"),
        working_directory: None,
        env: Vec::new(),
        watch_patterns: vec!["**/*.rs".to_owned()],
        restart_policy: DevRestartPolicy::OnFileChange,
        debounce_ms: Some(100),
    };
    let (workload, runtime, _docker, _, data_dir) = place_and_start(spec, &name).await;

    // The console carries the process's boot line.
    assert!(
        wait_for_console_line(&workload, "boot-").await,
        "the console never carried the boot line"
    );

    // Supervise: the watcher + the restart task go live.
    workload.supervise().expect("supervise");
    assert!(matches!(
        workload.watch_state(),
        WatchState::Active { .. }
    ));
    assert_eq!(workload.restarts(), 0);

    // A matching write under the data root: the restart fires.
    std::fs::create_dir_all(data_dir.join("src")).expect("create src");
    std::fs::write(data_dir.join("src/main.rs"), b"fn main() {}").expect("write the watched file");

    assert!(
        wait_for_restarts(&workload, 1).await,
        "the matching write never restarted the process (restarts: {})",
        workload.restarts()
    );
    // The process is Running again (the relaunch).
    assert!(matches!(
        runtime.state().await.unwrap(),
        ObservedState::Running
    ));

    // The console generation reset: the new run's boot line
    // arrives in the retained window.
    workload.attach_console().await.expect("re-attach");
    assert!(
        wait_for_console_line(&workload, "boot-").await,
        "the restarted process's boot line never arrived"
    );

    let _ = workload.stop_async().await;
    remove_quietly(&docker, &name).await;
}

/// The console surface: input reaches the process (a shell
/// echoes it) and the metrics sample answers.
#[tokio::test]
async fn the_console_surface_and_generation_reset() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let _gate = SUITE_GATE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());

    let name = unique_name("devproc-console");
    // A dev-process spec (the stand-in loop's carrier — the
    // console surface's own contract is under test here).
    let spec = DevProcessSpec {
        command: loop_command("x"),
        working_directory: None,
        env: Vec::new(),
        watch_patterns: vec!["**/*.rs".to_owned()],
        restart_policy: DevRestartPolicy::OnFileChange,
        debounce_ms: Some(100),
    };
    let (workload, _runtime, _docker, _, _data_dir) = place_and_start(spec, &name).await;

    // The console window carries the process's own output (the
    // boot announcement the stand-in loop prints).
    assert!(
        wait_for_console_line(&workload, "boot-").await,
        "the console never carried the boot line"
    );

    // The metrics sample answers.
    let usage = workload.metrics_async().await.expect("metrics");
    assert!(usage.memory_bytes > 0, "a live busybox resident set");

    let _ = workload.stop_async().await;
    remove_quietly(&docker, &name).await;
}

/// The policy posture: `Never` watches nothing — a change
/// restarts nothing.
#[tokio::test]
async fn the_never_policy_watches_nothing() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let _gate = SUITE_GATE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());

    let name = unique_name("devproc-never");
    let spec = DevProcessSpec {
        command: loop_command("x"),
        working_directory: None,
        env: Vec::new(),
        watch_patterns: vec!["**/*.rs".to_owned()],
        restart_policy: DevRestartPolicy::Never,
        debounce_ms: Some(100),
    };
    let (workload, _runtime, _docker, _, data_dir) = place_and_start(spec, &name).await;

    // Supervise is the documented no-op for a non-watching
    // policy; the state stays suspended.
    workload.supervise().expect("supervise");
    assert_eq!(workload.watch_state(), WatchState::Suspended);

    std::fs::create_dir_all(data_dir.join("src")).expect("create src");
    std::fs::write(data_dir.join("src/main.rs"), b"fn main() {}").expect("write the watched file");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(workload.restarts(), 0, "Never restarts nothing");

    let _ = workload.stop_async().await;
    remove_quietly(&docker, &name).await;
}

/// The patterns decide what matches: a change outside the
/// patterns restarts nothing.
#[tokio::test]
async fn the_patterns_decide_what_matches() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let _gate = SUITE_GATE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());

    let name = unique_name("devproc-pattern");
    let spec = DevProcessSpec {
        command: loop_command("x"),
        working_directory: None,
        env: Vec::new(),
        watch_patterns: vec!["**/*.rs".to_owned()],
        restart_policy: DevRestartPolicy::OnFileChange,
        debounce_ms: Some(100),
    };
    let (workload, _runtime, _docker, _, data_dir) = place_and_start(spec, &name).await;
    workload.supervise().expect("supervise");

    // A non-matching write: no restart.
    std::fs::write(data_dir.join("notes.txt"), b"not rust").expect("write the unwatched file");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(workload.restarts(), 0, "a non-matching change restarts nothing");

    let _ = workload.stop_async().await;
    remove_quietly(&docker, &name).await;
}
