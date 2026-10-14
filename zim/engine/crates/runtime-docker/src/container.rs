//! Per-instance container lifecycle: create/start/stop/kill/state,
//! with Wings-parity semantics (recreate-on-start,
//! graceful-then-kill stops, 404 tolerance).

use std::collections::HashMap;
use std::time::Duration;

use bollard::models::{
    ContainerCreateBody, ContainerInspectResponse, ContainerStateStatusEnum, HostConfig,
    HostConfigLogConfig, PortBinding as DockerPortBinding,
};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, KillContainerOptionsBuilder, RemoveContainerOptionsBuilder,
    StartContainerOptions, StopContainerOptionsBuilder, WaitContainerOptionsBuilder,
};
use bollard::Docker;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;

use zim_engine_application::supervision::{InstanceConfig, ObservedState, RuntimeError};

use crate::{is_not_found, map_error, DockerSettings, PullPolicy};

/// The capabilities Wings drops from every instance container
/// (`environment/docker/container.go` `CapDrop`), verbatim.
pub const DOCKER_CAP_DROP: &[&str] = &[
    "setpcap",
    "mknod",
    "audit_write",
    "net_raw",
    "dac_override",
    "fowner",
    "fsetid",
    "net_bind_service",
    "sys_chroot",
    "setfcap",
];

/// The fixed CFS period Wings uses (100 ms).
const CPU_PERIOD: i64 = 100_000;

// ── Observations ──────────────────────────────────────────────────

/// The observed state of one container (by name).
///
/// A missing container is `Offline { None, false }` (an instance
/// that was never started — or whose container was reaped — is not
/// running). Backend failures surface as errors; the supervision
/// layer turns `Unavailable` into observed `Unknown`.
pub(crate) async fn state(docker: &Docker, name: &str) -> Result<ObservedState, RuntimeError> {
    match inspect(docker, name).await {
        Ok(Some(inspect)) => Ok(state_from_inspect(&inspect)),
        Ok(None) => Ok(ObservedState::Offline {
            exit_code: None,
            oom_killed: false,
        }),
        Err(e) => Err(e),
    }
}

/// The last-known exit state `(exit_code, oom_killed)`.
pub(crate) async fn exit_state(
    docker: &Docker,
    name: &str,
) -> Result<(Option<i32>, bool), RuntimeError> {
    match inspect(docker, name).await {
        Ok(Some(inspect)) => {
            let state = inspect.state.as_ref();
            let exit_code = state
                .and_then(|s| s.exit_code)
                .map(|code| i32::try_from(code).unwrap_or(-1));
            let oom_killed = state.is_some_and(|s| s.oom_killed.unwrap_or(false));
            Ok((exit_code, oom_killed))
        }
        Ok(None) => Ok((None, false)),
        Err(e) => Err(e),
    }
}

fn state_from_inspect(inspect: &ContainerInspectResponse) -> ObservedState {
    let state = inspect.state.as_ref();
    let running = state.is_some_and(|s| s.running.unwrap_or(false));
    if running {
        return ObservedState::Running {
            started_at: state
                .and_then(|s| s.started_at.as_deref())
                .and_then(parse_docker_timestamp)
                .unwrap_or_else(Utc::now),
            pid: state
                .and_then(|s| s.pid)
                .and_then(|p| u32::try_from(p).ok()),
        };
    }
    // Docker reports `restarting` for auto-restart policies; ZIM sets
    // none, so treat it as still-running-observed (defensive).
    let status = state.and_then(|s| s.status);
    if status == Some(ContainerStateStatusEnum::RESTARTING) {
        return ObservedState::Running {
            started_at: state
                .and_then(|s| s.started_at.as_deref())
                .and_then(parse_docker_timestamp)
                .unwrap_or_else(Utc::now),
            pid: state
                .and_then(|s| s.pid)
                .and_then(|p| u32::try_from(p).ok()),
        };
    }
    ObservedState::Offline {
        exit_code: state
            .and_then(|s| s.exit_code)
            .map(|code| i32::try_from(code).unwrap_or(-1)),
        oom_killed: state.is_some_and(|s| s.oom_killed.unwrap_or(false)),
    }
}

/// Parse Docker's RFC3339 timestamps (nanosecond precision).
fn parse_docker_timestamp(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Inspect a container; `Ok(None)` when it does not exist.
async fn inspect(
    docker: &Docker,
    name: &str,
) -> Result<Option<ContainerInspectResponse>, RuntimeError> {
    match docker
        .inspect_container(
            name,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
    {
        Ok(inspect) => Ok(Some(inspect)),
        Err(e) if is_not_found(&e) => Ok(None),
        Err(e) => Err(map_error("container inspect", e)),
    }
}

// ── Start (recreate-on-start, Wings's OnBeforeStart) ──────────────

/// Start the instance's container: ensure the image, remove any
/// existing container (so the persisted configuration wins), create
/// it with the Wings-parity spec, and start it.
pub(crate) async fn start(
    docker: &Docker,
    name: &str,
    cfg: &InstanceConfig,
    settings: &DockerSettings,
    network_gateway: Option<std::net::Ipv4Addr>,
) -> Result<(), RuntimeError> {
    // The data directory must exist before the bind is created
    // (Docker would otherwise create it root-owned).
    if let Err(e) = std::fs::create_dir_all(&cfg.data_dir) {
        return Err(RuntimeError::InvalidConfig(format!(
            "cannot create data dir {}: {e}",
            cfg.data_dir.display()
        )));
    }

    ensure_image(docker, &cfg.image, settings.pull_policy).await?;

    // Destroy + recreate (Wings's OnBeforeStart): a container that
    // drifted from the persisted row never survives a start.
    remove_container(docker, name).await;

    let body = build_create_body(cfg, settings, network_gateway);
    docker
        .create_container(
            Some(CreateContainerOptionsBuilder::new().name(name).build()),
            body,
        )
        .await
        .map_err(|e| map_error("container create", e))?;

    docker
        .start_container(name, None::<StartContainerOptions>)
        .await
        .map_err(|e| map_error("container start", e))?;
    Ok(())
}

/// Remove a container (force, with volumes); a missing container is
/// fine (Wings's 404-tolerant destroy).
async fn remove_container(docker: &Docker, name: &str) {
    let _ = docker
        .remove_container(
            name,
            Some(
                RemoveContainerOptionsBuilder::new()
                    .force(true)
                    .v(true)
                    .build(),
            ),
        )
        .await;
}

/// Ensure the image exists per the pull policy, draining the pull
/// progress stream to completion (Wings's `ensureImageExists`).
async fn ensure_image(
    docker: &Docker,
    image: &str,
    policy: PullPolicy,
) -> Result<(), RuntimeError> {
    let present = docker
        .inspect_image(image)
        .await
        .map(|_| true)
        .or_else(|e| {
            if is_not_found(&e) {
                Ok(false)
            } else {
                Err(map_error("image inspect", e))
            }
        })?;

    match policy {
        PullPolicy::Never if !present => {
            return Err(RuntimeError::NotFound(format!(
                "image {image} is not present locally and the pull policy is Never"
            )));
        }
        PullPolicy::IfMissing if present => return Ok(()),
        PullPolicy::Always => {}
        _ => {}
    }

    // Split "repo:tag" for the pull options (default tag "latest").
    let (from_image, tag) = match image.rsplit_once(':') {
        Some((repo, tag)) if !tag.contains('/') => (repo.to_owned(), tag.to_owned()),
        _ => (image.to_owned(), "latest".to_owned()),
    };

    let options = bollard::query_parameters::CreateImageOptionsBuilder::new()
        .from_image(from_image.as_str())
        .tag(tag.as_str())
        .build();
    let mut stream = docker.create_image(Some(options), None, None);
    while let Some(item) = stream.next().await {
        item.map_err(|e| map_error("image pull", e))?;
        // Progress entries are drained, not surfaced: M7's streaming
        // module will forward them to the console.
    }
    Ok(())
}

/// Build the container-create body from the runtime-facing config
/// (the Wings `container.Config`/`HostConfig` mapping).
fn build_create_body(
    cfg: &InstanceConfig,
    settings: &DockerSettings,
    network_gateway: Option<std::net::Ipv4Addr>,
) -> ContainerCreateBody {
    // Env entries, with Wings's 127.0.0.1 → network-gateway rewrite
    // (`environment/docker/container.go`: SERVER_IP must address the
    // instance network, not the container's own loopback).
    let env: Vec<String> = cfg
        .env
        .iter()
        .map(|(key, value)| {
            if key == "SERVER_IP" && value == "127.0.0.1" {
                if let Some(gateway) = network_gateway {
                    return format!("{key}={gateway}");
                }
            }
            format!("{key}={value}")
        })
        .collect();

    // Exposed ports + bindings: host and container port map 1:1
    // (Pterodactyl convention).
    let mut exposed_ports = Vec::with_capacity(cfg.port_bindings.len());
    let mut port_bindings = HashMap::new();
    for binding in &cfg.port_bindings {
        let key = format!(
            "{}/{}",
            binding.container_port,
            binding.protocol.docker_suffix()
        );
        exposed_ports.push(key.clone());
        port_bindings.insert(
            key,
            Some(vec![DockerPortBinding {
                host_ip: Some(binding.host_ip.to_string()),
                host_port: Some(binding.host_port.to_string()),
            }]),
        );
    }

    let labels: HashMap<String, String> = cfg.labels.iter().cloned().collect();

    // Resource limits (Wings's `Limits().AsContainerResources()`).
    let memory = nonzero(cfg.limits.memory.0 as i64);
    let swap = cfg.limits.swap.0 as i64;
    let memory_swap = match (memory, swap) {
        (Some(mem), 0) => Some(mem),             // swap disabled
        (Some(mem), extra) => Some(mem + extra), // swap = extra allowance
        (None, _) => None,                       // unlimited memory
    };
    let cpu_quota = nonzero(cfg.limits.cpu_millicores as i64).map(|mc| mc * 100);

    let host_config = HostConfig {
        memory,
        memory_swap,
        cpu_period: cpu_quota.map(|_| CPU_PERIOD),
        cpu_quota,
        cpuset_cpus: cfg.limits.threads.clone(),
        blkio_weight: nonzero(cfg.limits.io_weight),
        oom_kill_disable: Some(cfg.limits.oom_disabled),
        pids_limit: None,
        binds: Some(vec![format!(
            "{}:{}",
            cfg.data_dir.display(),
            cfg.container_data_dir.display()
        )]),
        tmpfs: Some(HashMap::from([(
            "/tmp".to_owned(),
            format!("rw,exec,nosuid,size={}M", settings.tmpfs_size_mib),
        )])),
        port_bindings: Some(port_bindings),
        cap_drop: Some(DOCKER_CAP_DROP.iter().map(|s| s.to_string()).collect()),
        security_opt: Some(vec!["no-new-privileges".to_owned()]),
        network_mode: Some(settings.network_name.clone()),
        log_config: Some(HostConfigLogConfig {
            typ: Some("json-file".to_owned()),
            config: Some(HashMap::from([(
                "max-size".to_owned(),
                settings.log_max_size.clone(),
            )])),
        }),
        // M18: the per-instance rootfs posture — `None` keeps the
        // engine's hardened read-only default; the spec-driven
        // database images that must write their runtime dirs
        // (MySQL/MariaDB's /run/mysqld) carry `Some(false)`.
        readonly_rootfs: Some(cfg.readonly_rootfs.unwrap_or(true)),
        ..Default::default()
    };

    ContainerCreateBody {
        hostname: Some(cfg.hostname.clone()),
        // M18: the per-instance container user — `None` keeps the
        // Wings `988:988` posture; the managed-database images
        // carry their root-entrypoint user so their entrypoints can
        // chown the bind-mounted data directory before
        // self-demoting.
        user: cfg
            .user
            .clone()
            .or_else(|| Some(settings.container_user.clone())),
        attach_stdin: Some(true),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        open_stdin: Some(true),
        tty: Some(true),
        env: Some(env),
        cmd: Some(cfg.command.clone()),
        entrypoint: cfg.entrypoint.clone(),
        image: Some(cfg.image.clone()),
        labels: Some(labels),
        exposed_ports: Some(exposed_ports),
        host_config: Some(host_config),
        ..Default::default()
    }
}

fn nonzero<T: Default + PartialEq>(value: T) -> Option<T> {
    if value == T::default() {
        None
    } else {
        Some(value)
    }
}

// ── Stop / kill ───────────────────────────────────────────────────

/// Graceful stop bounded by `deadline` (Wings's `WaitForStop` with
/// `terminate = true`): `docker stop` (SIGTERM, escalate to SIGKILL
/// at the deadline), then wait for the not-running report; a stuck
/// wait gets a belt-and-braces kill.
pub(crate) async fn stop(
    docker: &Docker,
    name: &str,
    deadline: Duration,
) -> Result<(), RuntimeError> {
    match inspect(docker, name).await? {
        None => return Ok(()), // nothing to stop
        Some(inspect) => {
            let state = inspect.state.as_ref();
            if !state.is_some_and(|s| s.running.unwrap_or(false)) {
                return Ok(()); // already stopped
            }
        }
    }

    let timeout_secs = deadline.as_secs().clamp(1, 600) as i32;
    docker
        .stop_container(
            name,
            Some(StopContainerOptionsBuilder::new().t(timeout_secs).build()),
        )
        .await
        .map_err(|e| map_error("container stop", e))?;

    // Wait for the daemon's not-running report, bounded by the same
    // deadline; on timeout, force the kill.
    wait_not_running(docker, name, deadline, "container wait (stop)").await
}

/// Wait for the daemon's not-running report, bounded by `deadline`;
/// on timeout, force the kill (the escalation of Wings's
/// `WaitForStop` with `terminate = true`).
async fn wait_not_running(
    docker: &Docker,
    name: &str,
    deadline: Duration,
    operation: &'static str,
) -> Result<(), RuntimeError> {
    let mut wait = docker.wait_container(
        name,
        Some(
            WaitContainerOptionsBuilder::new()
                .condition("not-running")
                .build(),
        ),
    );
    match tokio::time::timeout(deadline, wait.next()).await {
        Ok(Some(Ok(_))) => Ok(()),
        Ok(Some(Err(e))) if is_not_found(&e) => Ok(()),
        // A wait response that carries an exit report (bollard maps
        // `status_code > 0` + an error field to this variant) means
        // the container exited — exactly the outcome we are waiting
        // for. The embedded "error" is docker's description of the
        // exit (e.g. a kill), not a transport failure.
        Ok(Some(Err(e @ bollard::errors::Error::DockerContainerWaitError { .. }))) => {
            let _ = e;
            Ok(())
        }
        Ok(Some(Err(e))) => Err(map_error(operation, e)),
        // Stream ended without an item: treat as stopped.
        Ok(None) => Ok(()),
        Err(_) => {
            docker
                .kill_container(
                    name,
                    Some(KillContainerOptionsBuilder::new().signal("SIGKILL").build()),
                )
                .await
                .map_err(|e| map_error("container kill (wait escalation)", e))?;
            Ok(())
        }
    }
}

/// Immediate kill (Wings's `Terminate`: SIGKILL + wait).
pub(crate) async fn kill(docker: &Docker, name: &str) -> Result<(), RuntimeError> {
    match inspect(docker, name).await? {
        None => return Ok(()),
        Some(inspect) => {
            let state = inspect.state.as_ref();
            if !state.is_some_and(|s| s.running.unwrap_or(false)) {
                return Ok(());
            }
        }
    }

    docker
        .kill_container(
            name,
            Some(KillContainerOptionsBuilder::new().signal("SIGKILL").build()),
        )
        .await
        .map_err(|e| map_error("container kill", e))?;

    // The SIGKILL is delivered; the (bounded) wait lets state settle.
    wait_not_running(
        docker,
        name,
        Duration::from_secs(15),
        "container wait (kill)",
    )
    .await
}

/// Remove a container regardless of state (cleanup paths: tests and
/// the future instance-deletion job).
pub(crate) async fn remove_for_cleanup(docker: &Docker, name: &str) {
    remove_container(docker, name).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;
    use std::path::PathBuf;
    use zim_engine_domain::instance::ResourceLimits;
    use zim_engine_domain::{Bytes, InstanceId};

    fn test_config() -> InstanceConfig {
        InstanceConfig {
            instance_id: InstanceId::new(),
            container_name: "test-uuid".to_owned(),
            hostname: "test-uuid".to_owned(),
            image: "busybox:latest".to_owned(),
            entrypoint: None,
            command: vec!["sleep".to_owned(), "600".to_owned()],
            env: vec![
                ("SERVER_MEMORY".to_owned(), "512".to_owned()),
                ("SERVER_PORT".to_owned(), "25565".to_owned()),
                ("SERVER_IP".to_owned(), "127.0.0.1".to_owned()),
            ],
            limits: ResourceLimits {
                memory: Bytes::from_mib(512),
                swap: Bytes::from_bytes(0),
                disk: Bytes::from_gib(5),
                io_weight: 500,
                cpu_millicores: 1500,
                threads: Some("0-1".to_owned()),
                oom_disabled: false,
            },
            port_bindings: vec![zim_engine_application::supervision::PortBinding {
                host_ip: "127.0.0.1".parse::<IpAddr>().unwrap(),
                host_port: 25565,
                container_port: 25565,
                protocol: Default::default(),
            }],
            data_dir: PathBuf::from("/tmp/zim-test-data"),
            container_data_dir: PathBuf::from("/home/container"),
            labels: vec![("Service".to_owned(), "ZIM".to_owned())],
            crash_detection_enabled: true,
            user: None,
            readonly_rootfs: None,
        }
    }

    #[test]
    fn create_body_maps_wings_parity() {
        let settings = DockerSettings::default();
        let body = build_create_body(
            &test_config(),
            &settings,
            Some("172.20.0.1".parse().unwrap()),
        );

        assert_eq!(body.hostname.as_deref(), Some("test-uuid"));
        assert_eq!(body.user.as_deref(), Some("988:988"));
        assert_eq!(body.tty, Some(true));
        assert_eq!(body.open_stdin, Some(true));
        assert_eq!(body.image.as_deref(), Some("busybox:latest"));
        assert_eq!(body.cmd, Some(vec!["sleep".to_owned(), "600".to_owned()]));
        assert_eq!(
            body.exposed_ports.as_deref(),
            Some(["25565/tcp".to_owned()].as_slice())
        );
        // The 127.0.0.1 → gateway rewrite.
        assert!(body
            .env
            .as_ref()
            .unwrap()
            .iter()
            .any(|e| e == "SERVER_IP=172.20.0.1"));

        let host = body.host_config.as_ref().unwrap();
        assert_eq!(host.memory, Some(512 * 1024 * 1024));
        assert_eq!(host.memory_swap, Some(512 * 1024 * 1024)); // swap disabled
        assert_eq!(host.cpu_quota, Some(150_000));
        assert_eq!(host.cpu_period, Some(100_000));
        assert_eq!(host.cpuset_cpus.as_deref(), Some("0-1"));
        assert_eq!(host.blkio_weight, Some(500));
        assert_eq!(host.readonly_rootfs, Some(true));
        assert_eq!(host.network_mode.as_deref(), Some("pterodactyl0"));
        assert_eq!(host.cap_drop.as_deref().unwrap().len(), 10);
        assert!(host
            .security_opt
            .as_deref()
            .unwrap()
            .contains(&"no-new-privileges".to_owned()));
        assert_eq!(
            host.binds.as_deref().unwrap().first().map(String::as_str),
            Some("/tmp/zim-test-data:/home/container")
        );
        assert!(host.tmpfs.as_ref().unwrap().contains_key("/tmp"));
        assert!(host
            .port_bindings
            .as_ref()
            .unwrap()
            .contains_key("25565/tcp"));
    }

    #[test]
    fn per_instance_user_and_rootfs_overrides_win() {
        // M18: the spec-driven database posture — a root-entrypoint
        // user and a writable rootfs — overrides the Wings defaults
        // exactly when the instance config carries them.
        let settings = DockerSettings::default();
        let mut cfg = test_config();
        cfg.user = Some("0:0".to_owned());
        cfg.readonly_rootfs = Some(false);
        let body = build_create_body(&cfg, &settings, None);
        assert_eq!(body.user.as_deref(), Some("0:0"));
        assert_eq!(
            body.host_config.as_ref().unwrap().readonly_rootfs,
            Some(false)
        );

        // The unset posture keeps the engine defaults (the
        // game-server path is unchanged by M18).
        let body = build_create_body(&test_config(), &settings, None);
        assert_eq!(body.user.as_deref(), Some("988:988"));
        assert_eq!(
            body.host_config.as_ref().unwrap().readonly_rootfs,
            Some(true)
        );
    }

    #[test]
    fn unlimited_limits_map_to_none() {
        let settings = DockerSettings::default();
        let mut cfg = test_config();
        cfg.limits = ResourceLimits::default();
        let body = build_create_body(&cfg, &settings, None);
        let host = body.host_config.as_ref().unwrap();
        assert_eq!(host.memory, None);
        assert_eq!(host.memory_swap, None);
        assert_eq!(host.cpu_quota, None);
        assert_eq!(host.blkio_weight, None);
        // No gateway: SERVER_IP stays literal.
        assert!(body
            .env
            .as_ref()
            .unwrap()
            .iter()
            .any(|e| e == "SERVER_IP=127.0.0.1"));
    }

    #[test]
    fn image_tag_splitting() {
        // Covered through ensure_image's options construction; the
        // split logic is exercised in the integration tests via real
        // pulls. This pins the pure splitting cases:
        fn split(image: &str) -> (String, String) {
            match image.rsplit_once(':') {
                Some((repo, tag)) if !tag.contains('/') => (repo.to_owned(), tag.to_owned()),
                _ => (image.to_owned(), "latest".to_owned()),
            }
        }
        assert_eq!(split("busybox:latest"), ("busybox".into(), "latest".into()));
        assert_eq!(split("busybox"), ("busybox".into(), "latest".into()));
        assert_eq!(
            split("ghcr.io/org/img:1.2"),
            ("ghcr.io/org/img".into(), "1.2".into())
        );
        assert_eq!(
            split("registry.io:5000/img"),
            ("registry.io:5000/img".into(), "latest".into())
        );
    }
}
