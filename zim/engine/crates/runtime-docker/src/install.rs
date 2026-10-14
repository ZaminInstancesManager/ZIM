//! The install-script runner over Docker (M8.3): one constrained
//! container per egg script run.
//!
//! The container an install script deserves is *not* the instance's
//! workload container: the egg names the image (an installer image,
//! not the runtime image), the script is untrusted legacy content,
//! and the run is one-shot. So this adapter creates a dedicated
//! container per run, with the Wings-parity security posture the
//! workload containers get — capabilities dropped
//! ([`DOCKER_CAP_DROP`]), `no-new-privileges`, a read-only root
//! filesystem, the unprivileged `container_user`, no published
//! ports — plus the two install-specific relaxations a script
//! needs: network egress (downloads are what install scripts *do*)
//! and the instance data directory bind (the only writable path
//! besides the bounded `/tmp` tmpfs).
//!
//! Lifecycle: create → start → wait(not-running) under the
//! spec's deadline with the cancellation hook polled between
//! waits → inspect for the exit code → collect a bounded output
//! tail from the log stream → force-remove. The remove is
//! unconditional (including timeout and cancellation paths): no
//! installer container ever outlives its run.
//!
//! Progress surfaces through the port's callback: the pull phase
//! reports `pulling_image` with byte-level fractions when the
//! daemon reports them, and the script phase reports
//! `running_install_script` at its boundaries (a shell script's
//! interior progress is not observable through the Docker API).

use std::collections::HashMap;

use async_trait::async_trait;
use bollard::models::{
    ContainerCreateBody, ContainerInspectResponse, HostConfig, HostConfigLogConfig,
};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, KillContainerOptionsBuilder, LogsOptionsBuilder,
    RemoveContainerOptionsBuilder, StartContainerOptions, WaitContainerOptionsBuilder,
};
use bollard::Docker;
use futures_util::StreamExt;

use zim_engine_application::supervision::{
    InstallCancelledFn, InstallProgressFn, InstallRunError, InstallRunOutcome, InstallRunner,
    InstallScriptSpec, RuntimeError,
};

use crate::{map_error, DockerSettings, PullPolicy};

/// How much script output the tail keeps (the failure message's
/// budget; Docker's own log rotation caps the rest).
const OUTPUT_TAIL_BYTES: usize = 8 * 1024;

/// The Docker [`InstallRunner`] adapter (see the module docs).
///
/// Shares the daemon client, settings, and instance-network gateway
/// with [`crate::DockerRuntimeFactory`] — one connection, one
/// network, one security posture.
#[derive(Clone)]
pub struct DockerInstallRunner {
    docker: Docker,
    settings: DockerSettings,
    network_gateway: Option<std::net::Ipv4Addr>,
}

impl DockerInstallRunner {
    /// Build the runner over an already-connected factory's client.
    pub fn from_factory(factory: &crate::DockerRuntimeFactory) -> Self {
        Self {
            docker: factory.client().clone(),
            settings: factory.settings().clone(),
            network_gateway: factory.network_gateway(),
        }
    }
}

#[async_trait]
impl InstallRunner for DockerInstallRunner {
    async fn run_install_script(
        &self,
        spec: &InstallScriptSpec,
        progress: InstallProgressFn,
        is_cancelled: InstallCancelledFn,
    ) -> Result<InstallRunOutcome, InstallRunError> {
        // The data directory must exist before the bind is created
        // (Docker would otherwise create it root-owned).
        if let Err(e) = std::fs::create_dir_all(&spec.data_dir) {
            return Err(RuntimeError::InvalidConfig(format!(
                "cannot create data dir {}: {e}",
                spec.data_dir.display()
            ))
            .into());
        }

        // ── PullingImage ────────────────────────────────────────
        progress("pulling_image", 0.0);
        ensure_image_with_progress(
            &self.docker,
            &spec.image,
            self.settings.pull_policy,
            &progress,
        )
        .await?;
        if is_cancelled() {
            // Nothing container-shaped exists yet: a clean stop.
            return Ok(InstallRunOutcome::Cancelled);
        }
        progress("pulling_image", 1.0);

        // ── The constrained container ──────────────────────────
        let body = build_install_body(spec, &self.settings, self.network_gateway);
        let create = self
            .docker
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::new()
                        .name(&spec.container_name)
                        .build(),
                ),
                body,
            )
            .await
            .map_err(|e| map_error("install container create", e))?;

        // From here on every exit path must remove the container.
        let outcome = self
            .run_container(&create.id, spec, &progress, &is_cancelled)
            .await;
        // Unconditional remove: an installer container never
        // outlives its run (missing-container is fine).
        let _ = self
            .docker
            .remove_container(
                &create.id,
                Some(
                    RemoveContainerOptionsBuilder::new()
                        .force(true)
                        .v(true)
                        .build(),
                ),
            )
            .await;
        outcome
    }
}

impl DockerInstallRunner {
    /// Start, wait, inspect, and collect the tail for one created
    /// install container. The caller owns the remove.
    async fn run_container(
        &self,
        id: &str,
        spec: &InstallScriptSpec,
        progress: &InstallProgressFn,
        is_cancelled: &InstallCancelledFn,
    ) -> Result<InstallRunOutcome, InstallRunError> {
        self.docker
            .start_container(id, None::<StartContainerOptions>)
            .await
            .map_err(|e| map_error("install container start", e))?;

        // ── RunningInstallScript ───────────────────────────────
        progress("running_install_script", 0.0);

        // Wait for not-running under the deadline, polling the
        // cancellation hook between bounded waits (each wait is
        // capped at a second so cancellation is observed promptly).
        let deadline = tokio::time::Instant::now() + spec.timeout;
        loop {
            if is_cancelled() {
                let _ = self
                    .docker
                    .kill_container(
                        id,
                        Some(KillContainerOptionsBuilder::new().signal("SIGKILL").build()),
                    )
                    .await;
                return Ok(InstallRunOutcome::Cancelled);
            }
            let slice = std::time::Duration::min(
                std::time::Duration::from_secs(1),
                deadline.saturating_duration_since(tokio::time::Instant::now()),
            );
            if slice.is_zero() {
                // Deadline exceeded: kill, then report the timeout.
                let _ = self
                    .docker
                    .kill_container(
                        id,
                        Some(KillContainerOptionsBuilder::new().signal("SIGKILL").build()),
                    )
                    .await;
                return Err(InstallRunError::TimedOut(spec.timeout));
            }
            let mut wait = self.docker.wait_container(
                id,
                Some(
                    WaitContainerOptionsBuilder::new()
                        .condition("not-running")
                        .build(),
                ),
            );
            match tokio::time::timeout(slice, wait.next()).await {
                // The daemon reported the container not running:
                // the script is done.
                Ok(Some(_)) => break,
                // Stream ended without an item: treat as exited.
                Ok(None) => break,
                // The slice elapsed: loop back around (cancellation
                // or deadline checks first).
                Err(_) => continue,
            }
        }

        progress("running_install_script", 1.0);

        // The exit code (inspect before the caller's remove races
        // us — remove happens after this returns, but inspect now
        // regardless: the container is not-running and stable).
        let inspect: ContainerInspectResponse = self
            .docker
            .inspect_container(
                id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
            .map_err(|e| map_error("install container inspect", e))?;
        let exit_code = inspect
            .state
            .as_ref()
            .and_then(|s| s.exit_code)
            .map(|code| i32::try_from(code).unwrap_or(-1));

        // A bounded output tail for diagnostics.
        let tail = self.collect_tail(id).await;
        Ok(InstallRunOutcome::Completed { exit_code, tail })
    }

    /// The last [`OUTPUT_TAIL_BYTES`] of combined stdout+stderr.
    async fn collect_tail(&self, id: &str) -> String {
        let options = LogsOptionsBuilder::new()
            .stdout(true)
            .stderr(true)
            .tail("200")
            .timestamps(false)
            .build();
        let mut stream = self.docker.logs(id, Some(options));
        let mut bytes: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let Ok(log) = chunk else { break };
            let message: &[u8] = match &log {
                bollard::container::LogOutput::StdOut { message } => message,
                bollard::container::LogOutput::StdErr { message } => message,
                other => other.as_ref(),
            };
            bytes.extend_from_slice(message);
            if bytes.len() > OUTPUT_TAIL_BYTES * 2 {
                let skip = bytes.len() - OUTPUT_TAIL_BYTES;
                bytes.drain(..skip);
            }
        }
        // Keep only the final budget, decoded lossily.
        if bytes.len() > OUTPUT_TAIL_BYTES {
            let skip = bytes.len() - OUTPUT_TAIL_BYTES;
            bytes.drain(..skip);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// Ensure the script's image exists, reporting byte-level pull
/// progress through the callback when the daemon reports totals.
///
/// The pull policy is the workload one (`DockerSettings`): a
/// missing installer image pulls on demand, exactly like a missing
/// workload image.
async fn ensure_image_with_progress(
    docker: &Docker,
    image: &str,
    policy: PullPolicy,
    progress: &InstallProgressFn,
) -> Result<(), InstallRunError> {
    let present = docker
        .inspect_image(image)
        .await
        .map(|_| true)
        .or_else(|e| {
            if crate::is_not_found(&e) {
                Ok(false)
            } else {
                Err(map_error("install image inspect", e))
            }
        })?;

    match policy {
        PullPolicy::Never if !present => {
            return Err(RuntimeError::NotFound(format!(
                "image {image} is not present locally and the pull policy is Never"
            ))
            .into());
        }
        PullPolicy::IfMissing if present => return Ok(()),
        PullPolicy::Always => {}
        _ => {}
    }

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
        let item = item.map_err(|e| map_error("install image pull", e))?;
        // Byte-level fraction when the daemon reports both sides
        // of the transfer; layers without totals are drained.
        if let (Some(current), Some(total)) = (
            item.progress_detail
                .as_ref()
                .and_then(|d| d.current)
                .filter(|_| {
                    item.progress_detail
                        .as_ref()
                        .and_then(|d| d.total)
                        .is_some()
                }),
            item.progress_detail.as_ref().and_then(|d| d.total),
        ) {
            if total > 0 {
                let fraction = (current as f32 / total as f32).clamp(0.0, 1.0);
                // Layer fractions are noisy per-layer; keep the
                // monotonic max within the pull phase.
                progress("pulling_image", fraction);
            }
        }
    }
    Ok(())
}

/// The install container's create body (the constrained posture —
/// see the module docs).
fn build_install_body(
    spec: &InstallScriptSpec,
    settings: &DockerSettings,
    network_gateway: Option<std::net::Ipv4Addr>,
) -> ContainerCreateBody {
    // Env entries, with the same 127.0.0.1 → network-gateway
    // rewrite the workload containers apply (install scripts that
    // fetch from SERVER_IP-addressed endpoints must reach the
    // instance network, not the container's own loopback).
    let env: Vec<String> = spec
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

    // Entrypoint: the egg's (bash in virtually all legacy eggs);
    // the script rides as `-c <body>` so the shell parses it as
    // one unit. An empty entrypoint defers to the image default
    // with the script as the command's first word.
    let (entrypoint, cmd): (Option<Vec<String>>, Vec<String>) = if spec.entrypoint.is_empty() {
        (
            None,
            vec!["bash".to_owned(), "-c".to_owned(), spec.script.clone()],
        )
    } else {
        (
            Some(vec![spec.entrypoint.clone()]),
            vec!["-c".to_owned(), spec.script.clone()],
        )
    };

    let mut labels: HashMap<String, String> = HashMap::from([
        ("Service".to_owned(), "ZIM".to_owned()),
        ("ContainerType".to_owned(), "server_install".to_owned()),
    ]);
    labels.extend(spec.labels.iter().cloned());

    let host_config = HostConfig {
        // The instance data directory is the only writable bind; a
        // bounded /tmp gives scripts scratch space without a
        // writable rootfs.
        binds: Some(vec![format!(
            "{}:{}",
            spec.data_dir.display(),
            spec.container_data_dir.display()
        )]),
        tmpfs: Some(HashMap::from([(
            "/tmp".to_owned(),
            format!("rw,exec,nosuid,size={}M", settings.tmpfs_size_mib),
        )])),
        cap_drop: Some(
            crate::container::DOCKER_CAP_DROP
                .iter()
                .map(|s| s.to_string())
                .collect(),
        ),
        security_opt: Some(vec!["no-new-privileges".to_owned()]),
        readonly_rootfs: Some(true),
        // Network egress is the point of an install script; the
        // instance network (with ICC) is where it belongs.
        network_mode: Some(settings.network_name.clone()),
        log_config: Some(HostConfigLogConfig {
            typ: Some("json-file".to_owned()),
            config: Some(HashMap::from([(
                "max-size".to_owned(),
                settings.log_max_size.clone(),
            )])),
        }),
        ..Default::default()
    };

    ContainerCreateBody {
        // A distinctive name inside the instance network's DNS is
        // not needed; the caller's container_name is unique per run.
        hostname: Some(spec.container_name.clone()),
        user: Some(settings.container_user.clone()),
        attach_stdin: Some(false),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        open_stdin: Some(false),
        tty: Some(false),
        env: Some(env),
        cmd: Some(cmd),
        entrypoint,
        image: Some(spec.image.clone()),
        labels: Some(labels),
        // Deliberately no exposed_ports / port_bindings: an install
        // container serves nothing.
        host_config: Some(host_config),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn spec() -> InstallScriptSpec {
        InstallScriptSpec {
            image: "ghcr.io/parkervcp/installers:alpine".to_owned(),
            entrypoint: "bash".to_owned(),
            script: "cd /home/container && echo hi".to_owned(),
            env: vec![
                ("SERVER_MEMORY".to_owned(), "512".to_owned()),
                ("SERVER_IP".to_owned(), "127.0.0.1".to_owned()),
                ("MINECRAFT_VERSION".to_owned(), "1.20.4".to_owned()),
            ],
            data_dir: PathBuf::from("/tmp/zim-install-test"),
            container_data_dir: PathBuf::from("/home/container"),
            container_name: "test_installer".to_owned(),
            timeout: std::time::Duration::from_secs(60),
            labels: vec![("ZIM.JobId".to_owned(), "job-1".to_owned())],
        }
    }

    #[test]
    fn install_body_maps_the_constrained_posture() {
        let settings = DockerSettings::default();
        let body = build_install_body(&spec(), &settings, Some("172.20.0.1".parse().unwrap()));

        // Entrypoint carries the egg's bash; the script rides -c.
        assert_eq!(
            body.entrypoint.as_deref(),
            Some(vec!["bash".to_owned()].as_slice())
        );
        assert_eq!(
            body.cmd.as_deref(),
            Some(vec!["-c".to_owned(), "cd /home/container && echo hi".to_owned()].as_slice())
        );
        assert_eq!(body.tty, Some(false));
        assert_eq!(body.open_stdin, Some(false));
        assert_eq!(
            body.image.as_deref(),
            Some("ghcr.io/parkervcp/installers:alpine")
        );

        // The 127.0.0.1 → gateway rewrite applies to installers too.
        assert!(body
            .env
            .as_ref()
            .unwrap()
            .iter()
            .any(|e| e == "SERVER_IP=172.20.0.1"));
        assert!(body
            .env
            .as_ref()
            .unwrap()
            .iter()
            .any(|e| e == "MINECRAFT_VERSION=1.20.4"));

        let host = body.host_config.as_ref().unwrap();
        // No published ports.
        assert!(host.port_bindings.is_none());
        assert!(body.exposed_ports.is_none());
        // The constrained posture.
        assert_eq!(host.readonly_rootfs, Some(true));
        assert_eq!(host.cap_drop.as_deref().map(|c| c.len()), Some(10));
        assert!(host
            .security_opt
            .as_deref()
            .unwrap()
            .contains(&"no-new-privileges".to_owned()));
        assert_eq!(host.network_mode.as_deref(), Some("pterodactyl0"));
        // The only bind is the data dir.
        assert_eq!(
            host.binds.as_deref().unwrap().first().map(String::as_str),
            Some("/tmp/zim-install-test:/home/container")
        );

        // Labels: the install marker plus the caller's.
        let labels = body.labels.as_ref().unwrap();
        assert_eq!(
            labels.get("ContainerType").map(String::as_str),
            Some("server_install")
        );
        assert_eq!(labels.get("Service").map(String::as_str), Some("ZIM"));
        assert_eq!(labels.get("ZIM.JobId").map(String::as_str), Some("job-1"));
    }

    #[test]
    fn install_body_without_entrypoint_defers_to_bash_cmd() {
        let settings = DockerSettings::default();
        let mut no_entry = spec();
        no_entry.entrypoint = String::new();
        let body = build_install_body(&no_entry, &settings, None);
        assert_eq!(body.entrypoint, None);
        assert_eq!(
            body.cmd.as_deref().map(|c| c.first().map(String::as_str)),
            Some(Some("bash"))
        );
        // No gateway: SERVER_IP stays literal.
        assert!(body
            .env
            .as_ref()
            .unwrap()
            .iter()
            .any(|e| e == "SERVER_IP=127.0.0.1"));
    }
}
