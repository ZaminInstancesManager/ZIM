//! The per-workload GenericContainer handle (M19): the §4
//! capability implementations over a live [`Runtime`].
//!
//! [`GenericContainerWorkload`] fuses one workload's runtime
//! handle, its instance configuration, its file root, and its
//! spec into the typed surface the SDK's capability traits
//! describe:
//!
//! - [`HealthCheckCapable`] — the spec's health check when it
//!   carries one, run over the placed host binding (the SDK's
//!   shared probe executor); the process's own presence
//!   otherwise (the transparent posture the kind documents —
//!   the container is the user's own, and without a check there
//!   is nothing else to ask);
//! - [`FilesystemCapable`] — the Wings file API over the
//!   instance's data directory;
//! - [`NetworkCapable`] — the multi-endpoint surface (the
//!   spec's named endpoints over the placed bindings; TCP and
//!   UDP both);
//! - [`ExecCapable`] — one-shot commands in the workload's
//!   context;
//! - [`MetricsCapable`] — the Docker-stats resource sample.
//!
//! # The sync↔async bridge
//!
//! Same shape as the sibling handles (M17-M19): the SDK's
//! capability traits are synchronous; the engine's [`Runtime`]
//! is async. The handle carries the engine runtime's
//! [`tokio::runtime::Handle`] and the trait implementations
//! `block_on` their async twins through it. **Calling the sync
//! trait methods from inside an async context on the same
//! runtime deadlocks** — async callers use the inherent
//! `*_async` methods.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use zim_engine_application::supervision::{
    ExecCommand as RuntimeExecCommand, ExecOutput as RuntimeExecOutput, InstanceConfig,
    ResourceUsage as RuntimeUsage, Runtime, RuntimeError,
};
use zim_engine_files::{FileError, Filesystem};
use zim_engine_workloads_sdk::errors::IntegrationError;
use zim_engine_workloads_sdk::traits::{
    ArchiveFormat as SdkFormat, ArchiveRef, Endpoint, ExecCapable, ExecCommand as SdkExecCommand,
    ExecOutput as SdkExecOutput, FilesystemCapable, Health, HealthCheckCapable, MetricsCapable,
    NetworkCapable, PortSpec, ResourceUsage as SdkUsage,
};

use crate::lifecycle::DEFAULT_STOP_DEADLINE;
use crate::spec::GenericContainerSpec;

/// The per-workload GenericContainer capability handle.
#[derive(Clone)]
pub struct GenericContainerWorkload {
    runtime: Arc<dyn Runtime>,
    cfg: Arc<RwLock<InstanceConfig>>,
    io: tokio::runtime::Handle,
    files: Filesystem,
    spec: GenericContainerSpec,
    next_host_port: Arc<AtomicU16>,
    stop_deadline: Duration,
    probe_timeout: Duration,
}

impl GenericContainerWorkload {
    /// Build the handle for one placed workload.
    pub fn new(
        runtime: Arc<dyn Runtime>,
        cfg: Arc<InstanceConfig>,
        io: tokio::runtime::Handle,
        spec: GenericContainerSpec,
    ) -> Self {
        let base = cfg
            .port_bindings
            .iter()
            .map(|binding| binding.host_port)
            .max()
            .map(|port| port.saturating_add(1))
            .filter(|port| *port >= FIRST_EXPOSED_HOST_PORT)
            .unwrap_or(FIRST_EXPOSED_HOST_PORT);
        Self {
            files: Filesystem::new(cfg.data_dir.clone()),
            spec,
            runtime,
            cfg: Arc::new(RwLock::new((*cfg).clone())),
            io,
            next_host_port: Arc::new(AtomicU16::new(base)),
            stop_deadline: DEFAULT_STOP_DEADLINE,
            probe_timeout: Duration::from_secs(10),
        }
    }

    /// Override the graceful-stop deadline.
    pub fn with_stop_deadline(mut self, deadline: Duration) -> Self {
        self.stop_deadline = deadline;
        self
    }

    /// Override the health probe's timeout.
    pub fn with_probe_timeout(mut self, timeout: Duration) -> Self {
        self.probe_timeout = timeout;
        self
    }

    /// The handle's spec.
    pub fn spec(&self) -> &GenericContainerSpec {
        &self.spec
    }

    /// The handle's file API.
    pub fn files(&self) -> &Filesystem {
        &self.files
    }

    /// A snapshot of the carried instance configuration.
    pub fn current_config(&self) -> InstanceConfig {
        self.cfg
            .read()
            .expect("the instance-config lock is only held briefly")
            .clone()
    }

    // ── The async twins ─────────────────────────────────────────

    /// (Re)create and start the workload's container from the
    /// carried configuration.
    pub async fn start_async(&self) -> Result<(), IntegrationError> {
        let cfg = self
            .cfg
            .read()
            .expect("the instance-config lock is only held briefly")
            .clone();
        self.runtime.start(&cfg).await.map_err(runtime_err)
    }

    /// Stop the workload: the runtime's graceful-then-forcible
    /// signal stop (the image's own convention — see
    /// [`crate::lifecycle`]).
    pub async fn stop_async(&self) -> Result<(), IntegrationError> {
        self.runtime
            .stop(self.stop_deadline)
            .await
            .map_err(runtime_err)
    }

    /// The health verdict: the spec's check when it carries one,
    /// the process's own presence otherwise.
    pub async fn health_async(&self) -> Result<Health, IntegrationError> {
        let Some(check) = self.spec.health_check.clone() else {
            // The transparent posture: without a check, the
            // process's presence is the verdict.
            let running = matches!(
                self.runtime.state().await.map_err(runtime_err)?,
                zim_engine_application::supervision::ObservedState::Running
            );
            return Ok(if running {
                Health::Healthy
            } else {
                Health::Unhealthy {
                    reason: "the container is not running (and the spec \
                             carries no health check to ask)"
                        .to_owned(),
                }
            });
        };
        let port = self
            .probe_host_port(&check)
            .ok_or_else(|| {
                IntegrationError::Runtime(format!(
                    "the health check's endpoint {:?} is not placed (no host \
                     binding carries it)",
                    check.port.as_deref().unwrap_or("primary")
                ))
            })?;
        let host = self.probe_host_ip();
        let mut bounded = check;
        bounded.timeout_ms = self.probe_timeout.min(bounded.timeout()).as_millis() as u64;
        Ok(zim_engine_workloads_sdk::run_health_probe(&bounded, host, port).await)
    }

    /// The host port the health check's logical endpoint placed.
    fn probe_host_port(&self, check: &zim_engine_workloads_sdk::HealthCheckSpec) -> Option<u16> {
        let cfg = self
            .cfg
            .read()
            .expect("the instance-config lock is only held briefly");
        let container_port = match check.port.as_deref() {
            Some(name) => self
                .spec
                .endpoints
                .iter()
                .find(|endpoint| endpoint.name == name)?
                .container_port,
            None => self.spec.endpoints.first()?.container_port,
        };
        cfg.port_bindings
            .iter()
            .find(|binding| {
                binding.container_port == container_port
                    && matches!(
                        binding.protocol,
                        zim_engine_application::supervision::PortProtocol::Tcp
                    )
            })
            .map(|binding| binding.host_port)
    }

    /// The host IP the probe connects through.
    fn probe_host_ip(&self) -> IpAddr {
        self.cfg
            .read()
            .expect("the instance-config lock is only held briefly")
            .port_bindings
            .first()
            .map(|binding| binding.host_ip)
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
    }

    /// One resource-usage sample.
    pub async fn metrics_async(&self) -> Result<SdkUsage, IntegrationError> {
        let usage = self.runtime.stats().await.map_err(runtime_err)?;
        Ok(to_sdk_usage(&usage))
    }

    /// Run one command inside the workload's container, bounded
    /// by `timeout`.
    pub async fn exec_argv_async(
        &self,
        argv: &[String],
        timeout: Duration,
    ) -> Result<RuntimeExecOutput, IntegrationError> {
        let command = RuntimeExecCommand {
            cmd: argv.to_vec(),
            env: Vec::new(),
            user: None,
            working_dir: None,
            timeout: Some(timeout),
        };
        self.runtime.exec(&command).await.map_err(runtime_err)
    }
}

impl HealthCheckCapable for GenericContainerWorkload {
    fn health(&self) -> Result<Health, IntegrationError> {
        // Sync bridge: see the module docs.
        self.io.block_on(self.health_async())
    }
}

impl FilesystemCapable for GenericContainerWorkload {
    fn list_dir(&self, path: &str) -> Result<Vec<String>, IntegrationError> {
        self.files
            .list_dir(path)
            .map(|entries| {
                entries
                    .into_iter()
                    .map(|entry| {
                        if entry.is_dir {
                            format!("{}/", entry.name)
                        } else {
                            entry.name
                        }
                    })
                    .collect()
            })
            .map_err(file_err)
    }

    fn read_file(&self, path: &str) -> Result<Vec<u8>, IntegrationError> {
        self.files.read_file(path).map_err(file_err)
    }

    fn write_file(&self, path: &str, content: &[u8]) -> Result<(), IntegrationError> {
        self.files.write_file(path, content).map_err(file_err)
    }

    fn delete(&self, path: &str) -> Result<(), IntegrationError> {
        self.files.delete(path).map_err(file_err)
    }

    fn archive(
        &self,
        dest: &str,
        paths: &[&str],
        format: SdkFormat,
    ) -> Result<ArchiveRef, IntegrationError> {
        let bytes = self
            .files
            .archive(dest, paths, to_files_format(format))
            .map_err(file_err)?;
        Ok(ArchiveRef {
            path: dest.to_owned(),
            bytes,
        })
    }

    fn extract(&self, archive_path: &str, dest: &str) -> Result<(), IntegrationError> {
        self.files
            .extract(archive_path, dest)
            .map(|_| ())
            .map_err(file_err)
    }
}

impl NetworkCapable for GenericContainerWorkload {
    fn endpoints(&self) -> Result<Vec<Endpoint>, IntegrationError> {
        let cfg = self
            .cfg
            .read()
            .expect("the instance-config lock is only held briefly")
            .clone();
        let placed: Vec<Endpoint> = self
            .spec
            .endpoints
            .iter()
            .filter_map(|endpoint| {
                let protocol = port_protocol(&endpoint.protocol);
                let binding = cfg.port_bindings.iter().find(|binding| {
                    binding.container_port == endpoint.container_port
                        && std::mem::discriminant(&binding.protocol)
                            == std::mem::discriminant(&protocol)
                })?;
                Some(Endpoint {
                    name: endpoint.name.clone(),
                    port: binding.host_port,
                })
            })
            .collect();
        let mut all = placed;
        for binding in &cfg.port_bindings {
            let known = all
                .iter()
                .any(|endpoint| endpoint.port == binding.host_port);
            if !known {
                all.push(Endpoint {
                    name: format!("port-{}", binding.container_port),
                    port: binding.host_port,
                });
            }
        }
        Ok(all)
    }

    fn expose_port(&self, spec: PortSpec) -> Result<Endpoint, IntegrationError> {
        let protocol = port_protocol(spec.protocol);
        let mut cfg = self
            .cfg
            .write()
            .expect("the instance-config lock is only held briefly");
        if let Some(existing) = cfg.port_bindings.iter().find(|binding| {
            binding.container_port == spec.container_port
                && std::mem::discriminant(&binding.protocol) == std::mem::discriminant(&protocol)
        }) {
            return Ok(Endpoint {
                name: spec.name,
                port: existing.host_port,
            });
        }
        let host_port = self.next_host_port.fetch_add(1, Ordering::Relaxed);
        let host_ip = cfg
            .port_bindings
            .first()
            .map(|binding| binding.host_ip)
            .unwrap_or_else(|| "127.0.0.1".parse().expect("a valid loopback IP"));
        cfg.port_bindings
            .push(zim_engine_application::supervision::PortBinding {
                host_ip,
                host_port,
                container_port: spec.container_port,
                protocol,
            });
        Ok(Endpoint {
            name: spec.name,
            port: host_port,
        })
    }

    fn unexpose_port(&self, endpoint: &Endpoint) -> Result<(), IntegrationError> {
        let mut cfg = self
            .cfg
            .write()
            .expect("the instance-config lock is only held briefly");
        let before = cfg.port_bindings.len();
        cfg.port_bindings
            .retain(|binding| binding.host_port != endpoint.port);
        if cfg.port_bindings.len() == before {
            return Err(IntegrationError::Runtime(format!(
                "the endpoint {:?} (port {}) is not placed",
                endpoint.name, endpoint.port
            )));
        }
        Ok(())
    }
}

impl ExecCapable for GenericContainerWorkload {
    fn exec(&self, command: SdkExecCommand) -> Result<SdkExecOutput, IntegrationError> {
        // Sync bridge: see the module docs.
        self.io.block_on(async {
            let runtime_command = RuntimeExecCommand {
                cmd: split_command(&command.command),
                env: Vec::new(),
                user: None,
                working_dir: None,
                timeout: Some(command.timeout),
            };
            let output = self.runtime.exec(&runtime_command).await.map_err(runtime_err)?;
            Ok(SdkExecOutput {
                exit_code: output.exit_code.and_then(|code| i32::try_from(code).ok()),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
        })
    }
}

impl MetricsCapable for GenericContainerWorkload {
    fn metrics(&self) -> Result<SdkUsage, IntegrationError> {
        // Sync bridge: see the module docs.
        self.io.block_on(self.metrics_async())
    }
}

/// The first host port the handle's expose path allocates from.
const FIRST_EXPOSED_HOST_PORT: u16 = 24700;

/// Split one command line into argv (the SDK's ExecCommand
/// carries a single command string; the runtime speaks argv).
fn split_command(command: &str) -> Vec<String> {
    command
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

/// Map a runtime error into the integration vocabulary.
fn runtime_err(error: RuntimeError) -> IntegrationError {
    match error {
        RuntimeError::NotRunning(reason) => IntegrationError::NotRunning {
            operation: "runtime call",
            reason,
        },
        other => IntegrationError::Runtime(other.to_string()),
    }
}

/// Map a file-API error into the integration vocabulary.
fn file_err(error: FileError) -> IntegrationError {
    IntegrationError::File(error.to_string())
}

/// Map the SDK's archive format onto the file API's.
fn to_files_format(format: SdkFormat) -> zim_engine_files::ArchiveFormat {
    match format {
        SdkFormat::Tar => zim_engine_files::ArchiveFormat::Tar,
        SdkFormat::TarGz => zim_engine_files::ArchiveFormat::TarGz,
        SdkFormat::Zip => zim_engine_files::ArchiveFormat::Zip,
    }
}

/// Resolve the protocol vocabulary onto the runtime's.
fn port_protocol(protocol: &str) -> zim_engine_application::supervision::PortProtocol {
    match protocol {
        "udp" => zim_engine_application::supervision::PortProtocol::Udp,
        _ => zim_engine_application::supervision::PortProtocol::Tcp,
    }
}

/// Map the runtime's resource sample onto the SDK's §4 shape.
fn to_sdk_usage(usage: &RuntimeUsage) -> SdkUsage {
    let cpu = match (
        usage.cpu_usage_total_ns,
        usage.system_cpu_total_ns,
        usage.online_cpus,
    ) {
        (Some(process), Some(system), Some(cpus)) if system > 0 && cpus > 0 => {
            let fraction = process as f64 / system as f64;
            let cores = cpus as f64;
            (fraction * cores).clamp(0.0, cores) as f32
        }
        _ => 0.0,
    };
    let network = usage
        .network_rx_bytes
        .unwrap_or(0)
        .saturating_add(usage.network_tx_bytes.unwrap_or(0));
    SdkUsage {
        cpu,
        memory_bytes: usage.memory_usage_bytes.unwrap_or(0),
        network_bytes: network,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lines_split_into_argv() {
        assert_eq!(
            split_command("echo hello world"),
            vec!["echo", "hello", "world"]
        );
        assert_eq!(split_command("  ls  -la  "), vec!["ls", "-la"]);
        assert!(split_command("").is_empty());
    }
}
