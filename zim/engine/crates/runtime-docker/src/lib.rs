//! The Docker runtime adapter (M6).
//!
//! Implements [`Runtime`] / [`RuntimeFactory`] from
//! `zim_engine_application::supervision` over the local Docker
//! daemon (unix socket / named pipe), preserving Wings's container
//! semantics:
//!
//! - the `pterodactyl0` bridge network is created on demand with
//!   Wings's bridge options (`environment/docker.go`);
//! - the per-instance container is *recreated* on every start
//!   (`OnBeforeStart`'s destroy-and-recreate, so the persisted row
//!   always wins over a drifted container);
//! - stop is graceful-then-forcible bounded by a deadline
//!   (`WaitForStop` with `terminate = true`);
//! - stop/kill tolerate a missing container (404-as-Ok, Wings's
//!   `client.IsErrNotFound` handling);
//! - the console surface is a persistent bollard attach (M7.1):
//!   `attach_console` opens it once per run and `send_input` writes
//!   through its stdin side (see `attach.rs`).
//!
//! # Deviations from Wings, documented per site
//!
//! - The network is created with an auto-allocated IPAM subnet
//!   instead of Wings's fixed `172.18.0.0/16` (auto-assignment
//!   avoids conflicts with existing bridge networks; the fixed
//!   subnet is a Pterodactyl deployment convention, not a
//!   behavioral requirement).
//! - IPv6 is off by default (Wings enables it with a v6 subnet);
//!   several host configurations reject IPv6 bridge networking.
//!   Configurable via [`DockerSettings::enable_ipv6`].
//! - The container user defaults to Wings's `988:988` unprivileged
//!   uid:gid, configurable via [`DockerSettings::container_user`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod attach;
mod container;
mod exec;
mod install;
mod network;
mod stats;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bollard::Docker;

use zim_engine_application::supervision::{
    ExecCommand, ExecOutput, InstanceConfig, ObservedState, ResourceUsage, RuntimeError,
    RuntimeKind,
};
use zim_engine_application::supervision::{Runtime, RuntimeFactory};

pub use container::DOCKER_CAP_DROP;
pub use install::DockerInstallRunner;

/// Remove an instance's container (force, with volumes); missing
/// containers are fine. Cleanup path for tests and the future
/// instance-deletion job (M8).
pub async fn remove_container_force(docker: &bollard::Docker, name: &str) {
    container::remove_for_cleanup(docker, name).await;
}

/// When the Docker adapter pulls instance images.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PullPolicy {
    /// Pull only when the image is missing locally (Wings's
    /// `ensureImageExists` behavior).
    #[default]
    IfMissing,
    /// Never pull; a missing image fails the start.
    Never,
    /// Always pull before start (fresh-image deployments).
    Always,
}

/// The Docker adapter's tunables (Wings's `config.Docker` +
/// `config.System` subset that the runtime cares about).
#[derive(Debug, Clone)]
pub struct DockerSettings {
    /// The instance network's name (Wings's
    /// `Docker.Network.Interface`, default `pterodactyl0`).
    pub network_name: String,
    /// The network driver (`bridge`).
    pub network_driver: String,
    /// Whether the network is created with IPv6 enabled (Wings
    /// default: on; ZIM default: off — see the crate docs).
    pub enable_ipv6: bool,
    /// Inter-container communication on the instance network (Wings's
    /// `EnableICC`, default on — BungeeCord-style server-to-server
    /// traffic).
    pub enable_icc: bool,
    /// The instance network's MTU.
    pub network_mtu: u16,
    /// The user containers run as (`uid:gid`; Wings's default
    /// `988:988`).
    pub container_user: String,
    /// The `/tmp` tmpfs size cap in MiB (Wings's `TmpfsSize`).
    pub tmpfs_size_mib: u32,
    /// The container log driver's max-size cap.
    pub log_max_size: String,
    /// The image pull policy.
    pub pull_policy: PullPolicy,
    /// Host-side root for instance data directories (the supervisor
    /// is given the same root; the factory only carries it for
    /// diagnostics).
    pub data_root: PathBuf,
    /// Upper bound for a single Docker API round-trip used by
    /// `exec` (the daemon's own calls use the client default).
    pub api_timeout: Duration,
}

impl Default for DockerSettings {
    fn default() -> Self {
        Self {
            network_name: "pterodactyl0".to_owned(),
            network_driver: "bridge".to_owned(),
            enable_ipv6: false,
            enable_icc: true,
            network_mtu: 1500,
            container_user: "988:988".to_owned(),
            tmpfs_size_mib: 100,
            log_max_size: "10m".to_owned(),
            pull_policy: PullPolicy::default(),
            data_root: PathBuf::from("zim_data"),
            api_timeout: Duration::from_secs(30),
        }
    }
}

/// The Docker [`RuntimeFactory`]: connects the daemon client and
/// prepares the instance network once, then hands out per-instance
/// [`DockerRuntime`] handles.
pub struct DockerRuntimeFactory {
    docker: Docker,
    settings: DockerSettings,
    /// The instance network's IPv4 gateway (the `SERVER_IP=127.0.0.1`
    /// rewrite target; `None` when the network has no v4 gateway).
    network_gateway: Option<std::net::Ipv4Addr>,
}

impl DockerRuntimeFactory {
    /// Connect to the local daemon, verify it responds, and ensure
    /// the instance network exists.
    ///
    /// Fails with [`RuntimeError::Unavailable`] when no daemon is
    /// reachable — the composition root falls back to the null
    /// runtime (degraded, API-only mode) on that error.
    pub async fn connect(settings: DockerSettings) -> Result<Self, RuntimeError> {
        let docker = Docker::connect_with_local_defaults()
            .map_err(|e| RuntimeError::unavailable(format!("docker client: {e}")))?;

        // A reachable-but-unresponsive daemon is the same failure
        // class (ping through the version endpoint).
        let version = tokio::time::timeout(Duration::from_secs(5), docker.version())
            .await
            .map_err(|_| RuntimeError::unavailable("docker daemon ping timed out"))?
            .map_err(|e| RuntimeError::unavailable(format!("docker daemon ping: {e}")))?;
        let _ = version; // reachability is all we verify

        let network_gateway = network::ensure_network(&docker, &settings)
            .await
            .map_err(|e| {
                if e.is_unavailable() {
                    e
                } else {
                    RuntimeError::Backend(format!("network setup failed: {e}"))
                }
            })?;

        Ok(Self {
            docker,
            settings,
            network_gateway,
        })
    }

    /// The daemon client (tests and diagnostics).
    pub fn client(&self) -> &Docker {
        &self.docker
    }

    /// The effective settings.
    pub fn settings(&self) -> &DockerSettings {
        &self.settings
    }

    /// The instance network's IPv4 gateway (shared with the
    /// install runner so installer containers get the same
    /// SERVER_IP rewrite).
    pub fn network_gateway(&self) -> Option<std::net::Ipv4Addr> {
        self.network_gateway
    }
}

#[async_trait]
impl RuntimeFactory for DockerRuntimeFactory {
    async fn create(&self, cfg: &InstanceConfig) -> Result<Arc<dyn Runtime>, RuntimeError> {
        Ok(Arc::new(DockerRuntime {
            docker: self.docker.clone(),
            name: cfg.container_name.clone(),
            settings: self.settings.clone(),
            network_gateway: self.network_gateway,
            console: attach::ConsoleState::shared(),
        }))
    }

    async fn check(&self) -> Result<(), RuntimeError> {
        let pong = tokio::time::timeout(Duration::from_secs(5), self.docker.ping())
            .await
            .map_err(|_| RuntimeError::unavailable("docker daemon ping timed out"))?
            .map_err(|e| RuntimeError::unavailable(format!("docker daemon ping: {e}")))?;
        let _ = pong; // "OK" — reachability is all we verify
        Ok(())
    }
}

/// The per-instance Docker [`Runtime`] handle: addresses the
/// instance's container by its legacy-UUID name.
pub struct DockerRuntime {
    docker: Docker,
    name: String,
    settings: DockerSettings,
    network_gateway: Option<std::net::Ipv4Addr>,
    /// The console attachment state (M7.1): the live attach's stdin
    /// writer, shared with the output pump that clears it on end.
    console: Arc<attach::ConsoleState>,
}

#[async_trait]
impl Runtime for DockerRuntime {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Container
    }

    async fn state(&self) -> Result<ObservedState, RuntimeError> {
        container::state(&self.docker, &self.name).await
    }

    async fn start(&self, cfg: &InstanceConfig) -> Result<(), RuntimeError> {
        container::start(
            &self.docker,
            &self.name,
            cfg,
            &self.settings,
            self.network_gateway,
        )
        .await
    }

    async fn stop(&self, deadline: Duration) -> Result<(), RuntimeError> {
        container::stop(&self.docker, &self.name, deadline).await
    }

    async fn kill(&self) -> Result<(), RuntimeError> {
        container::kill(&self.docker, &self.name).await
    }

    async fn exec(&self, cmd: &ExecCommand) -> Result<ExecOutput, RuntimeError> {
        exec::exec(&self.docker, &self.name, cmd, self.settings.api_timeout).await
    }

    async fn attach_console(
        &self,
    ) -> Result<zim_engine_application::supervision::ConsoleAttachment, RuntimeError> {
        attach::attach_console(&self.docker, &self.name, &self.console).await
    }

    async fn send_input(&self, input: &[u8]) -> Result<(), RuntimeError> {
        // Writes go through the persistent attachment opened by
        // `attach_console` (M7.1); without one the container is not
        // running and the input has nowhere to go.
        attach::send_input(&self.console, input).await
    }

    async fn exit_state(&self) -> Result<(Option<i32>, bool), RuntimeError> {
        container::exit_state(&self.docker, &self.name).await
    }

    async fn stats(&self) -> Result<ResourceUsage, RuntimeError> {
        stats::stats(&self.docker, &self.name).await
    }
}

/// Map a bollard error into the runtime vocabulary.
pub(crate) fn map_error(operation: &'static str, error: bollard::errors::Error) -> RuntimeError {
    match &error {
        bollard::errors::Error::DockerResponseServerError { status_code, .. }
            if *status_code == 404 =>
        {
            RuntimeError::NotFound(format!("{operation}: container or image not found"))
        }
        bollard::errors::Error::DockerResponseServerError {
            status_code,
            message,
        } => RuntimeError::Backend(format!("{operation}: docker {status_code}: {message}")),
        bollard::errors::Error::IOError { err } if err.kind() == std::io::ErrorKind::NotFound => {
            // The daemon socket itself is gone: unreachable, not
            // "container missing".
            RuntimeError::unavailable(format!("{operation}: docker socket unavailable"))
        }
        bollard::errors::Error::IOError { err }
            if err.kind() == std::io::ErrorKind::PermissionDenied =>
        {
            RuntimeError::unavailable(format!("{operation}: docker socket permission denied"))
        }
        other => RuntimeError::Backend(format!("{operation}: {other}")),
    }
}

/// Whether a bollard error is a 404 from the daemon.
pub(crate) fn is_not_found(error: &bollard::errors::Error) -> bool {
    matches!(
        error,
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}

/// The canonical crate name.
pub const CRATE_NAME: &str = "zim_engine_runtime_docker";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_is_set() {
        assert_eq!(CRATE_NAME, "zim_engine_runtime_docker");
    }

    #[test]
    fn wings_parity_defaults() {
        let settings = DockerSettings::default();
        assert_eq!(settings.network_name, "pterodactyl0");
        assert_eq!(settings.container_user, "988:988");
        assert_eq!(settings.tmpfs_size_mib, 100);
        assert!(settings.enable_icc);
        assert_eq!(settings.network_mtu, 1500);
        assert_eq!(settings.pull_policy, PullPolicy::IfMissing);
        // The cap-drop list matches Wings's HostConfig verbatim.
        assert_eq!(
            DOCKER_CAP_DROP,
            &[
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
            ]
        );
    }
}
