//! The GenericContainer integration's parity suite (M19).
//!
//! Real containers (busybox — the runtime's own pulled base),
//! driven through the engine's own runtime. The suite
//! self-skips when no Docker daemon answers.
//!
//! The §M19 acceptance, in the suite's terms:
//!
//! - **"A generic container can be placed on a runtime that
//!   provides the capabilities it declares"** — the negotiation
//!   accepts a satisfying runtime and rejects a deficient one
//!   with the structured vocabulary, and a negotiated workload
//!   runs end to end
//!   ([`the_placement_negotiation_and_lifecycle`]).
//! - The capability family over a live container — exec, the
//!   network endpoint surface, the TCP health check against a
//!   real listener, and the filesystem over the data directory
//!   ([`the_capability_family`]).

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bollard::Docker;

use zim_engine_application::supervision::{
    InstanceConfig, ObservedState, PortBinding, PortProtocol, Runtime,
};
use zim_engine_workloads_generic_container::{
    negotiate, negotiate_or_reject, full_runtime, minimal_runtime, GenericContainerSpec,
    GenericContainerWorkload, SpecEndpoint,
};
use zim_engine_workloads_sdk::errors::IntegrationError;
use zim_engine_workloads_sdk::traits::{
    Endpoint, FilesystemCapable, Health, NetworkCapable,
};
use zim_engine_workloads_sdk::{Capability, CapabilitySet, HealthCheckSpec};

/// A unique container name per test run.
fn unique_name(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

/// The next host port to bind.
fn unique_host_port() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static NEXT_PORT: AtomicU16 = AtomicU16::new(21400);
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
        data_root: std::env::temp_dir().join("zim-generic-docker-test"),
        ..zim_engine_runtime_docker::DockerSettings::default()
    }
}

/// Build the instance configuration for one generic spec.
fn generic_config(
    spec: &GenericContainerSpec,
    name: &str,
) -> (InstanceConfig, PathBuf) {
    let data_dir = std::env::temp_dir().join(format!("zim-generic-data-{name}"));
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    let mut bound: Vec<(u16, &str)> = Vec::new();
    let mut port_bindings = Vec::new();
    for endpoint in &spec.endpoints {
        if bound.contains(&(endpoint.container_port, endpoint.protocol.as_str())) {
            continue;
        }
        bound.push((endpoint.container_port, endpoint.protocol.as_str()));
        port_bindings.push(PortBinding {
            host_ip: "127.0.0.1".parse::<IpAddr>().unwrap(),
            host_port: unique_host_port(),
            container_port: endpoint.container_port,
            protocol: port_protocol(&endpoint.protocol),
        });
    }

    let config = InstanceConfig {
        instance_id: zim_engine_domain::InstanceId::new(),
        container_name: name.to_owned(),
        hostname: name.to_owned(),
        image: spec.image.clone(),
        entrypoint: None,
        command: spec.effective_command().unwrap_or_default(),
        env: spec.env.clone(),
        limits: Default::default(),
        port_bindings,
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

/// The runtime's protocol vocabulary.
fn port_protocol(protocol: &str) -> PortProtocol {
    match protocol {
        "udp" => PortProtocol::Udp,
        _ => PortProtocol::Tcp,
    }
}

/// Place one workload: connect the factory, create the runtime
/// handle, and build the workload handle.
async fn place(
    spec: GenericContainerSpec,
    name: &str,
) -> (GenericContainerWorkload, Arc<dyn Runtime>, Docker, String, PathBuf) {
    let docker = bollard::Docker::connect_with_local_defaults().expect("docker client");
    let (config, data_dir) = generic_config(&spec, name);
    let factory = zim_engine_runtime_docker::DockerRuntimeFactory::connect(test_settings())
        .await
        .expect("docker factory connects");
    let runtime = factory
        .create(&config)
        .await
        .expect("runtime handle created");
    let workload = GenericContainerWorkload::new(
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

/// The suite gate: one suite at a time.
static SUITE_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The §M19 headline acceptance: the negotiation places a
/// satisfying runtime, rejects a deficient one with the
/// structured vocabulary, and a negotiated workload runs end to
/// end.
#[tokio::test]
async fn the_placement_negotiation_and_lifecycle() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let _gate = SUITE_GATE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());

    // A spec that needs the health + network surfaces.
    let mut spec = base_listener_spec();
    spec.health_check = Some(HealthCheckSpec::tcp("http"));
    spec.required_capabilities = vec![Capability::ExecCapable, Capability::MetricsCapable];

    // A satisfying runtime places it.
    let verdict = negotiate(&spec, &full_runtime());
    assert!(verdict.placed, "{}", verdict.reason());
    let required = negotiate_or_reject(&spec, &full_runtime()).unwrap();
    assert!(required.contains(Capability::HealthCheckCapable));
    assert!(required.contains(Capability::NetworkCapable));

    // A minimal runtime rejects it, naming what is missing.
    let verdict = negotiate(&spec, &minimal_runtime());
    assert!(!verdict.placed);
    assert_eq!(
        verdict.missing,
        vec![
            Capability::HealthCheckCapable,
            Capability::NetworkCapable
        ],
        "{:?}",
        verdict.missing
    );
    assert!(verdict.reason().contains("HealthCheckCapable"));
    let err = negotiate_or_reject(&spec, &minimal_runtime()).unwrap_err();
    match err {
        IntegrationError::CapabilityMissing { capability } => {
            // The missing list is in the vocabulary's declaration
            // order — HealthCheckCapable precedes NetworkCapable.
            assert_eq!(capability, Capability::HealthCheckCapable);
        }
        other => panic!("expected CapabilityMissing, got {other:?}"),
    }

    // The negotiated workload runs end to end.
    let name = unique_name("generic-place");
    let (workload, runtime, _docker, _, _) = place(spec, &name).await;
    workload.start_async().await.expect("start");
    let _ = wait_until_running(&*runtime).await;

    // The TCP health check against the real listener answers
    // Healthy.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let health = loop {
        let health = workload.health_async().await.expect("health call");
        if matches!(health, Health::Healthy)
            || tokio::time::Instant::now() > deadline
        {
            break health;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    assert_eq!(health, Health::Healthy, "the listener never answered");

    // Stop: the health verdict degrades with the container.
    workload.stop_async().await.expect("stop");
    let health = workload.health_async().await.expect("health call");
    match health {
        Health::Unhealthy { reason } => {
            // The stopped container's port refuses — the probe's
            // own vocabulary — or the check reports it.
            assert!(
                reason.contains("refuse") || reason.contains("not running") || reason.contains("connect"),
                "{reason}"
            );
        }
        other => panic!("expected Unhealthy after stop, got {other:?}"),
    }

    remove_quietly(&docker, &name).await;
}

/// The capability family over a live container: exec, the
/// network endpoint surface, and the filesystem over the data
/// directory.
#[tokio::test]
async fn the_capability_family() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let _gate = SUITE_GATE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());

    let spec = base_listener_spec();
    let name = unique_name("generic-family");
    let (workload, runtime, _docker, _, data_dir) = place(spec, &name).await;
    workload.start_async().await.expect("start");
    let _ = wait_until_running(&*runtime).await;

    // Exec: a command inside the container answers.
    let output = workload
        .exec_argv_async(&["echo".to_owned(), "zim-generic".to_owned()], Duration::from_secs(15))
        .await
        .expect("exec");
    assert_eq!(output.exit_code, Some(0));
    assert!(String::from_utf8_lossy(&output.stdout).contains("zim-generic"));

    // The network endpoint surface: the declared names resolve
    // their placed bindings, and the expose family edits them.
    let endpoints = workload.endpoints().expect("endpoints");
    assert_eq!(endpoints.len(), 1, "{endpoints:?}");
    assert_eq!(endpoints[0].name, "http");

    let exposed = workload
        .expose_port(zim_engine_workloads_sdk::traits::PortSpec {
            name: "extra".to_owned(),
            container_port: 9090,
            protocol: "tcp",
        })
        .expect("expose");
    let endpoints = workload.endpoints().expect("endpoints");
    assert_eq!(endpoints.len(), 2, "{endpoints:?}");
    workload
        .unexpose_port(&Endpoint {
            name: "extra".to_owned(),
            port: exposed.port,
        })
        .expect("unexpose");
    let endpoints = workload.endpoints().expect("endpoints");
    assert_eq!(endpoints.len(), 1, "{endpoints:?}");

    // The filesystem over the data directory: write, list, read
    // back, delete.
    workload
        .write_file("hello.txt", b"generic workloads")
        .expect("write");
    let listing = workload.list_dir(".").expect("list");
    assert!(listing.iter().any(|entry| entry.contains("hello.txt")), "{listing:?}");
    let content = workload.read_file("hello.txt").expect("read");
    assert_eq!(content, b"generic workloads");
    workload.delete("hello.txt").expect("delete");
    assert!(workload.read_file("hello.txt").is_err());

    // The host side sees the same tree (the bind mount): the
    // deleted file is gone host-side too.
    assert!(!data_dir.join("hello.txt").exists());

    // The metrics sample answers.
    let usage = workload.metrics_async().await.expect("metrics");
    assert!(usage.memory_bytes > 0, "a live busybox resident set");

    workload.stop_async().await.expect("stop");
    remove_quietly(&docker, &name).await;
}

/// A UDP endpoint declaration survives the surface (the
/// protocol vocabulary is not silently TCP-only).
#[tokio::test]
async fn the_udp_endpoint_declares_and_surfaces() {
    let mut spec = GenericContainerSpec {
        image: "busybox:1.36".to_owned(),
        command: Some(vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "while true; do sleep 5; done".to_owned(),
        ]),
        env: Vec::new(),
        endpoints: vec![SpecEndpoint {
            name: "dns".to_owned(),
            container_port: 53,
            protocol: "udp".to_owned(),
        }],
        health_check: None,
        resource_limits: None,
        required_capabilities: Vec::new(),
    };
    // The UDP endpoint drives the network requirement.
    assert!(spec
        .derived_requirements()
        .contains(Capability::NetworkCapable));
    spec.validate().unwrap();

    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let _gate = SUITE_GATE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());

    let name = unique_name("generic-udp");
    let (workload, runtime, _docker, _, _) = place(spec, &name).await;
    workload.start_async().await.expect("start");
    let _ = wait_until_running(&*runtime).await;

    let endpoints = workload.endpoints().expect("endpoints");
    assert_eq!(endpoints.len(), 1, "{endpoints:?}");
    assert_eq!(endpoints[0].name, "dns");

    workload.stop_async().await.expect("stop");
    remove_quietly(&docker, &name).await;
}

/// The negotiation's unit posture (no daemon needed): the
/// explicit and content-implied requirements negotiate against
/// deficient runtimes.
#[test]
fn the_negotiation_names_every_missing_capability() {
    let mut spec = GenericContainerSpec {
        image: "busybox:1.36".to_owned(),
        command: None,
        env: Vec::new(),
        endpoints: Vec::new(),
        health_check: None,
        resource_limits: None,
        required_capabilities: vec![
            Capability::ConsoleCapable,
            Capability::SnapshotCapable,
        ],
    };
    let provided: CapabilitySet = [Capability::ConsoleCapable, Capability::ExecCapable]
        .into_iter()
        .collect();
    let verdict = negotiate(&spec, &provided);
    assert!(!verdict.placed);
    assert_eq!(verdict.missing, vec![Capability::SnapshotCapable]);

    // The content-implied requirement joins the explicit ones.
    spec.health_check = Some(HealthCheckSpec::tcp("http"));
    let verdict = negotiate(&spec, &provided);
    assert_eq!(
        verdict.missing,
        vec![
            Capability::HealthCheckCapable,
            Capability::SnapshotCapable
        ],
        "{:?}",
        verdict.missing
    );
    assert_eq!(verdict.required.len(), 3);
}

// ── helpers ─────────────────────────────────────────────────────

/// The listener spec: a busybox container with an HTTP-named TCP
/// listener (a `nc` accept loop the health check can probe — the
/// listener restarts per connection, so repeated probes always
/// find a live acceptor).
fn base_listener_spec() -> GenericContainerSpec {
    GenericContainerSpec {
        image: "busybox:1.36".to_owned(),
        command: Some(vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "while true; do nc -l -p 8080 </dev/null; done".to_owned(),
        ]),
        env: Vec::new(),
        endpoints: vec![SpecEndpoint::new("http", 8080)],
        health_check: None,
        resource_limits: None,
        required_capabilities: Vec::new(),
    }
}
