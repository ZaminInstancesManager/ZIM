//! The Website integration's parity suite (M19).
//!
//! Real containers (the static carrier's `nginx:1.27-alpine`),
//! driven through the engine's own runtime — the same posture
//! the Minecraft (M17) and Database (M18) parity suites take. The
//! suite self-skips when no Docker daemon answers, so it runs
//! wherever a daemon exists and nowhere else.
//!
//! The §M19 acceptance, in the suite's terms:
//!
//! - **"A static website can be deployed and served by ZIM"** —
//!   the install path bootstraps the release structure, the
//!   container serves it, and the deploy path switches a new
//!   version in atomically ([`the_static_website_acceptance`]).
//! - **The health check** — HTTP and TCP probes over the placed
//!   binding, with the unhealthy verdict's reason carrying its
//!   own explanation ([`the_health_check_family`]).
//! - **The deploy lifecycle** — alongside/switch/drain/retain, and
//!   the rollback window
//!   ([`the_deploy_lifecycle_and_rollback`]).
//! - **The network surface** — the http logical name resolves its
//!   placed binding, and the expose family edits it
//!   ([`the_network_endpoint_family`]).

use std::net::{IpAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bollard::Docker;
use futures_util::StreamExt;

use zim_engine_application::supervision::{
    InstanceConfig, ObservedState, PortBinding, PortProtocol, Runtime, RuntimeFactory,
};
use zim_engine_workloads_sdk::traits::{Health, NetworkCapable};
use zim_engine_workloads_website::{DeployOutcome, WebsiteSpec, WebsiteWorkload};

/// A unique container name per test run.
fn unique_name(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

/// The next host port to bind (tests run in parallel; a fixed
/// port collides across containers).
fn unique_host_port() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static NEXT_PORT: AtomicU16 = AtomicU16::new(21300);
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
        data_root: std::env::temp_dir().join("zim-web-docker-test"),
        ..zim_engine_runtime_docker::DockerSettings::default()
    }
}

/// Build the instance configuration for one website spec: the
/// spec's resolved argv, the static carrier's container posture,
/// and one port binding per distinct container port.
fn website_config(spec: &WebsiteSpec, name: &str) -> (InstanceConfig, PathBuf) {
    let data_dir = std::env::temp_dir().join(format!("zim-web-data-{name}"));
    std::fs::create_dir_all(&data_dir).expect("create data dir");

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
        container_data_dir: std::path::PathBuf::from("/home/container"),
        labels: vec![("Service".to_owned(), "ZIM".to_owned())],
        crash_detection_enabled: true,
        user: posture.user,
        readonly_rootfs: Some(posture.readonly_rootfs),
        cap_drop: None,
    };
    (config, data_dir)
}

/// Place one workload: connect the factory, create the runtime
/// handle, and build the workload handle.
async fn place(
    spec: WebsiteSpec,
    name: &str,
) -> (WebsiteWorkload, Arc<dyn Runtime>, Docker, String, PathBuf) {
    let docker = bollard::Docker::connect_with_local_defaults().expect("docker client");
    let (config, data_dir) = website_config(&spec, name);
    let factory = zim_engine_runtime_docker::DockerRuntimeFactory::connect(test_settings())
        .await
        .expect("docker factory connects");
    let runtime = factory
        .create(&config)
        .await
        .expect("runtime handle created");
    let workload = WebsiteWorkload::new(
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

/// One raw HTTP GET over a host port (the suite's own client —
/// the same minimal surface the health probe speaks).
fn http_get(port: u16, path: &str) -> Result<(u16, String), String> {
    let mut stream =
        TcpStream::connect(("127.0.0.1", port)).map_err(|err| format!("connect: {err}"))?;
    use std::io::{Read, Write};
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nUser-Agent: zim-parity\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("send: {err}"))?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|err| format!("read: {err}"))?;
    let text = String::from_utf8_lossy(&response).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| format!("no status line in {text:?}"))?;
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    Ok((status, body))
}

/// The suite gate: one suite at a time (the same posture the
/// M18 parity suite takes — the containers' boots serialize).
static SUITE_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The §M19 headline acceptance: a static website installed,
/// served, deployed (a new version switched in atomically), and
/// rolled back.
#[tokio::test]
async fn the_static_website_acceptance() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let _gate = SUITE_GATE.lock().await;

    let name = unique_name("web-static");
    let (workload, runtime, _docker, _, _) = place(default_spec_for_test(), &name).await;

    // Install: the configuration, the first release, the link.
    workload.install_async().await.expect("install");
    let current = workload.current_release().expect("current readable");
    assert!(current.is_some(), "the install switched a first release");

    // Start: the container serves the placeholder.
    workload.start_async().await.expect("start");
    let _ = wait_until_running(&*runtime).await;
    let port = workload
        .current_config()
        .port_bindings
        .first()
        .expect("one binding")
        .host_port;

    let served = wait_for_http(port, "/", "placeholder", Duration::from_secs(60));
    assert!(served, "the placeholder page never served");

    // Health: the default HTTP check answers Healthy.
    let health = workload.health_async().await.expect("health call");
    assert_eq!(health, Health::Healthy);

    // Deploy: a new version switched in atomically.
    let v2 = page("v2");
    let outcome: DeployOutcome = workload
        .deploy_async(&[("site/index.html".to_owned(), v2)])
        .await
        .expect("deploy");
    assert_eq!(
        workload
            .current_release()
            .expect("current readable")
            .as_deref(),
        Some(outcome.release_id.as_str())
    );
    let (_, body) = http_get(port, "/").expect("the deployed version serves");
    assert!(
        body.contains("v2"),
        "the served body should carry the deployed version: {body:?}"
    );

    // Rollback: the previous release returns.
    let rolled_back = workload.rollback_async().await.expect("rollback");
    assert_eq!(
        rolled_back.as_deref(),
        outcome.previous_release_id.as_deref(),
        "the rollback target is the deploy's own previous release"
    );
    let (_, body) = http_get(port, "/").expect("the rolled-back version serves");
    assert!(body.contains("placeholder"), "body: {body:?}");

    // The metrics sample answers.
    let usage = workload.metrics_async().await.expect("metrics");
    assert!(usage.memory_bytes > 0, "a live nginx resident set");

    // Teardown.
    let _ = runtime.stop(Duration::from_secs(30)).await;
    remove_quietly(&docker, &name).await;
}

/// The health check family: the HTTP probe's verdicts and the
/// TCP probe's, over the placed binding — including the
/// Unhealthy verdict's self-explaining reason.
#[tokio::test]
async fn the_health_check_family() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let _gate = SUITE_GATE.lock().await;

    // A TCP check against a stopped workload: the placement
    // carries the binding whether or not the container runs, so
    // the probe runs — and the stopped listener refuses (the
    // Unhealthy verdict's own self-explaining reason).
    let name = unique_name("web-health");
    let (workload, runtime, _docker, _, _) = place(
        WebsiteSpec {
            health_check: Some(zim_engine_workloads_sdk::HealthCheckSpec::tcp("http")),
            ..default_spec_for_test()
        },
        &name,
    )
    .await;
    let health = workload.health_async().await.expect("health call");
    match health {
        Health::Unhealthy { reason } => {
            assert!(reason.contains("connect"), "{reason}");
        }
        other => panic!("expected Unhealthy against a stopped listener, got {other:?}"),
    }

    // Start it: the TCP check answers Healthy.
    workload.install_async().await.expect("install");
    workload.start_async().await.expect("start");
    let _ = wait_until_running(&*runtime).await;
    let health = workload.health_async().await.expect("health call");
    assert_eq!(health, Health::Healthy);

    // An HTTP check against a missing path: Unhealthy, with the
    // status carried in the reason (nginx answers 404 for a
    // missing file — an honest verdict).
    let mut spec = default_spec_for_test();
    spec.health_check = Some(zim_engine_workloads_sdk::HealthCheckSpec::http(
        "http", "/absent",
    ));
    let name2 = unique_name("web-health2");
    let (workload2, runtime2, _docker, _, _) = place(spec, &name2).await;
    workload2.install_async().await.expect("install");
    workload2.start_async().await.expect("start");
    let _ = wait_until_running(&*runtime2).await;
    let deadline = Instant::now() + Duration::from_secs(60);
    let verdict = loop {
        match workload2.health_async().await.expect("health call") {
            Health::Unhealthy { reason } => break reason,
            _ if Instant::now() > deadline => {
                panic!("the missing-path check never answered Unhealthy")
            }
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    assert!(verdict.contains("404"), "{verdict}");

    let _ = runtime.stop(Duration::from_secs(30)).await;
    let _ = runtime2.stop(Duration::from_secs(30)).await;
    remove_quietly(&docker, &name).await;
    remove_quietly(&docker, &name2).await;
}

/// The deploy lifecycle's retention: repeated deploys keep the
/// rollback window bounded and prune the older releases.
#[tokio::test]
async fn the_deploy_lifecycle_and_rollback() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let _gate = SUITE_GATE.lock().await;

    let name = unique_name("web-deploy");
    let (workload, runtime, _docker, _, _) = place(default_spec_for_test(), &name).await;
    workload.install_async().await.expect("install");
    workload.start_async().await.expect("start");
    let _ = wait_until_running(&*runtime).await;
    let port = workload
        .current_config()
        .port_bindings
        .first()
        .expect("one binding")
        .host_port;

    // Deploy five versions; the retention (3) prunes the oldest.
    for version in 1..=5u32 {
        let body = format!("serve-me-{version}");
        let outcome = workload
            .deploy_async(&[("site/index.html".to_owned(), page(&body))])
            .await
            .expect("deploy");
        assert_eq!(
            workload
                .current_release()
                .expect("current readable")
                .as_deref(),
            Some(outcome.release_id.as_str())
        );
        let (_, served) = http_get(port, "/").expect("the deployed version serves");
        assert!(served.contains(&body), "version {version}: {served:?}");
    }

    // The install release was pruned away with the oldest deploys;
    // the retention window is exactly the last 3.
    let releases = workload.releases().expect("releases readable");
    assert_eq!(
        releases.len(),
        3,
        "the retention keeps exactly 3 releases: {releases:?}"
    );
    // The rollback target is the second-to-last deploy.
    let target = workload.rollback_async().await.expect("rollback");
    assert!(target.is_some());
    let (_, served) = http_get(port, "/").expect("the rolled-back version serves");
    assert!(served.contains("serve-me-4"), "{served:?}");

    // An empty deploy is refused with the structured spec error.
    let err = workload.deploy_async(&[]).await.expect_err("empty deploy");
    assert!(err.to_string().contains("at least one file"), "{err}");

    let _ = runtime.stop(Duration::from_secs(30)).await;
    remove_quietly(&docker, &name).await;
}

/// The network endpoint family: the http logical name resolves
/// its placed binding, the expose family edits it, and the
/// unexpose family removes it.
#[tokio::test]
async fn the_network_endpoint_family() {
    let Some(docker) = docker_or_skip().await else {
        return;
    };
    let _gate = SUITE_GATE.lock().await;

    let name = unique_name("web-net");
    let (workload, runtime, _docker, _, _) = place(default_spec_for_test(), &name).await;

    // Not running yet: the endpoints still surface (the placed
    // bindings exist whether or not the container does).
    let endpoints = workload.endpoints().expect("endpoints");
    assert_eq!(endpoints.len(), 1, "{endpoints:?}");
    assert_eq!(endpoints[0].name, "http");

    // Expose one more port: an idempotent edit surface.
    let exposed = workload
        .expose_port(zim_engine_workloads_sdk::traits::PortSpec {
            name: "extra".to_owned(),
            container_port: 8080,
            protocol: "tcp",
        })
        .expect("expose");
    assert_eq!(exposed.name, "extra");
    let again = workload
        .expose_port(zim_engine_workloads_sdk::traits::PortSpec {
            name: "extra".to_owned(),
            container_port: 8080,
            protocol: "tcp",
        })
        .expect("expose again");
    assert_eq!(again.port, exposed.port, "the expose is idempotent");

    let endpoints = workload.endpoints().expect("endpoints");
    let names: Vec<&str> = endpoints.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"http"), "{names:?}");
    // The expose-added binding surfaces under the fallback name
    // (the same surface the Database handle reports — the
    // expose's own return carries the requested name).
    assert!(names.contains(&"port-8080"), "{names:?}");

    // Unexpose: the binding is gone.
    workload
        .unexpose_port(&zim_engine_workloads_sdk::traits::Endpoint {
            name: "extra".to_owned(),
            port: exposed.port,
        })
        .expect("unexpose");
    let endpoints = workload.endpoints().expect("endpoints");
    assert_eq!(endpoints.len(), 1, "{endpoints:?}");

    let _ = runtime.stop(Duration::from_secs(30)).await;
    remove_quietly(&docker, &name).await;
}

/// The console-pump parity: the static carrier's access log
/// lands on the container's stdout (the `docker logs` surface the
/// console stream would carry).
#[tokio::test]
async fn the_access_log_lands_on_stdout() {
    if docker_or_skip().await.is_none() {
        return;
    }
    let _gate = SUITE_GATE.lock().await;

    let name = unique_name("web-logs");
    let (workload, runtime, docker, _, _) = place(default_spec_for_test(), &name).await;
    workload.install_async().await.expect("install");
    workload.start_async().await.expect("start");
    let _ = wait_until_running(&*runtime).await;
    let port = workload
        .current_config()
        .port_bindings
        .first()
        .expect("one binding")
        .host_port;

    let _ = http_get(port, "/").expect("the placeholder serves");

    // The access log carries the request (nginx writes it to
    // /dev/stdout; the container's log surface carries it).
    let deadline = Instant::now() + Duration::from_secs(30);
    let logged = loop {
        let logs = docker_logs(&docker, &name).await;
        if logs.contains("GET / ") {
            break true;
        }
        if Instant::now() > deadline {
            break false;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    assert!(
        logged,
        "the access log never carried the request; logs: {}",
        docker_logs(&docker, &name).await
    );

    let _ = runtime.stop(Duration::from_secs(30)).await;
    remove_quietly(&docker, &name).await;
}

// ── helpers ─────────────────────────────────────────────────────

/// The default spec, test-shaped.
fn default_spec_for_test() -> WebsiteSpec {
    zim_engine_workloads_website::default_spec()
}

/// One page carrying a marker.
fn page(marker: &str) -> Vec<u8> {
    format!("<!doctype html>\n<html><body><h1>{marker}</h1></body></html>\n").into_bytes()
}

/// Poll until the served body carries the marker (the listener's
/// own boot time).
fn wait_for_http(port: u16, path: &str, marker: &str, deadline: Duration) -> bool {
    let started = Instant::now();
    loop {
        if let Ok((_, body)) = http_get(port, path) {
            if body.contains(marker) {
                return true;
            }
        }
        if started.elapsed() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// The container's log tail (bollard's logs surface, drained to
/// a string — the same helper shape the M18 database parity
/// suite carries).
async fn docker_logs(docker: &Docker, name: &str) -> String {
    let options = bollard::query_parameters::LogsOptionsBuilder::new()
        .stdout(true)
        .stderr(true)
        .tail("50")
        .timestamps(false)
        .build();
    let mut stream = docker.logs(name, Some(options));
    let mut collected = String::new();
    while let Some(Ok(log)) = stream.next().await {
        let message: &[u8] = match &log {
            bollard::container::LogOutput::StdOut { message } => message,
            bollard::container::LogOutput::StdErr { message } => message,
            other => other.as_ref(),
        };
        collected.push_str(&String::from_utf8_lossy(message));
    }
    collected
}
