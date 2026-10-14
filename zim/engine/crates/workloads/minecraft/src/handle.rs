//! The per-workload Minecraft handle (M17): the §4 capability
//! implementations over a live [`Runtime`].
//!
//! [`MinecraftWorkload`] fuses one workload's runtime handle, its
//! instance configuration, its file root, and its spec into the
//! typed surface the SDK's capability traits describe:
//!
//! - [`ConsoleCapable`] — attach the console pump, tail the retained
//!   window, send input lines;
//! - [`FilesystemCapable`] — the Wings file API over the instance's
//!   data directory (archive/extract included);
//! - [`ExecCapable`] — one-shot commands in the workload's context;
//! - [`MetricsCapable`] — the Docker-stats resource sample;
//! - [`Migratable`] — the file-copy migration family: quiesce (the
//!   stop convention), checkpoint, finalize (relaunch), abort.
//!
//! # The sync↔async bridge
//!
//! The SDK's capability traits are synchronous (M16's shape); the
//! engine's [`Runtime`] is async. The handle carries the engine
//! runtime's [`tokio::runtime::Handle`] and the trait
//! implementations `block_on` their async twins through it.
//! **Calling the sync trait methods from inside an async context on
//! the same runtime deadlocks** — they are for the job-executor's
//! worker threads and `spawn_blocking` contexts, which is exactly
//! where the engine invokes them. Async callers use the inherent
//! `*_async` methods directly.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use zim_engine_application::supervision::{
    ExecCommand as RuntimeExecCommand, InstanceConfig, ObservedState,
    ResourceUsage as RuntimeUsage, Runtime, RuntimeError,
};
use zim_engine_files::{FileError, Filesystem};
use zim_engine_workloads_sdk::errors::IntegrationError;
use zim_engine_workloads_sdk::traits::{
    ArchiveFormat as SdkFormat, ArchiveRef, ConsoleCapable, ExecCapable,
    ExecCommand as SdkExecCommand, ExecOutput as SdkExecOutput, FilesystemCapable, MetricsCapable,
    Migratable, MigrationCheckpoint, MigrationPlan, ResourceUsage as SdkUsage,
};

use crate::console::ConsoleBuffer;
use crate::lifecycle::{lifecycle_plan, DEFAULT_STOP_DEADLINE};
use crate::spec::GameServerSpec;

/// How often the stop path polls for process exit.
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// The migration throughput assumption for the downtime estimate
/// (bytes/second; deliberately conservative so the surfaced window
/// over-promises time rather than under-promising it).
const MIGRATION_BYTES_PER_SECOND: f64 = 50.0 * 1024.0 * 1024.0;

/// The per-workload Minecraft capability handle.
///
/// Clone-cheap (every field is an `Arc`, a `Handle`, a `Copy`, or a
/// spec clone) so blocking-context callers can move a clone into
/// `spawn_blocking`.
#[derive(Clone)]
pub struct MinecraftWorkload {
    runtime: Arc<dyn Runtime>,
    cfg: Arc<InstanceConfig>,
    io: tokio::runtime::Handle,
    files: Filesystem,
    spec: GameServerSpec,
    console: Arc<ConsoleBuffer>,
    /// Whether a console pump is live for the current run (attach
    /// idempotence: Docker's attach is per-connection, and a second
    /// concurrent attach would duplicate every output line into the
    /// retention buffer).
    attached: Arc<AtomicBool>,
    stop_deadline: Duration,
}

impl MinecraftWorkload {
    /// Build the handle for one placed workload.
    pub fn new(
        runtime: Arc<dyn Runtime>,
        cfg: Arc<InstanceConfig>,
        io: tokio::runtime::Handle,
        spec: GameServerSpec,
    ) -> Self {
        Self {
            files: Filesystem::new(cfg.data_dir.clone()),
            spec,
            runtime,
            cfg,
            io,
            console: Arc::new(ConsoleBuffer::new()),
            attached: Arc::new(AtomicBool::new(false)),
            stop_deadline: DEFAULT_STOP_DEADLINE,
        }
    }

    /// Override the graceful-stop deadline.
    pub fn with_stop_deadline(mut self, deadline: Duration) -> Self {
        self.stop_deadline = deadline;
        self
    }

    /// The handle's spec.
    pub fn spec(&self) -> &GameServerSpec {
        &self.spec
    }

    /// The handle's file API.
    pub fn files(&self) -> &Filesystem {
        &self.files
    }

    /// The retained console lines (the `ConsoleCapable` window).
    pub fn console(&self) -> &ConsoleBuffer {
        &self.console
    }

    // ── The async twins ─────────────────────────────────────────

    /// Attach the console pump: opens the runtime attachment and
    /// spawns the task that fills the retention buffer.
    ///
    /// Idempotent per run: a second call while a pump is live is a
    /// no-op (Docker's attach is per-connection; a parallel second
    /// attach would duplicate every output line). The pump clears
    /// the attached marker when the run ends (the workload exits),
    /// so a new run attaches fresh. Must be called while the
    /// workload is running; a stopped workload answers
    /// [`IntegrationError::NotRunning`].
    pub async fn attach_console(&self) -> Result<(), IntegrationError> {
        if self.attached.load(Ordering::Acquire) {
            return Ok(());
        }
        let attachment = self.runtime.attach_console().await.map_err(runtime_err)?;
        self.attached.store(true, Ordering::Release);
        let console = Arc::clone(&self.console);
        let attached = Arc::clone(&self.attached);
        // The consumer runs until the attachment ends (the workload
        // exits; the runtime's pump flushes its tail first), then
        // re-arms the idempotence marker for the next run.
        tokio::spawn(async move {
            let mut lines = attachment.lines;
            while let Some(line) = lines.recv().await {
                console.push_line(line);
            }
            attached.store(false, Ordering::Release);
        });
        Ok(())
    }

    /// Send one console input line (the runtime newline-terminates).
    pub async fn send_input_async(&self, line: &str) -> Result<(), IntegrationError> {
        self.runtime
            .send_input(line.as_bytes())
            .await
            .map_err(runtime_err)
    }

    /// Run one one-shot command in the workload's context (the
    /// engine-facing argv form).
    pub async fn exec_async(
        &self,
        command: RuntimeExecCommand,
    ) -> Result<zim_engine_application::supervision::ExecOutput, IntegrationError> {
        self.runtime.exec(&command).await.map_err(runtime_err)
    }

    /// One resource-usage sample (the Docker stats capability).
    pub async fn metrics_async(&self) -> Result<SdkUsage, IntegrationError> {
        let usage = self.runtime.stats().await.map_err(runtime_err)?;
        Ok(to_sdk_usage(&usage))
    }

    /// (Re)create and start the workload's container from the
    /// carried configuration (the runtime's Wings-parity
    /// destroy-and-recreate).
    pub async fn start_async(&self) -> Result<(), IntegrationError> {
        self.attached.store(false, Ordering::Release);
        self.console.clear();
        self.runtime.start(&self.cfg).await.map_err(runtime_err)
    }

    /// Wait for the workload to exit, polling up to `deadline`.
    ///
    /// Answers the observed `(exit_code, oom_killed)` when the exit
    /// landed inside the deadline, `None` when it did not.
    pub async fn wait_for_exit(
        &self,
        deadline: Duration,
    ) -> Result<Option<(Option<i32>, bool)>, IntegrationError> {
        let started = tokio::time::Instant::now();
        loop {
            let state = self.runtime.state().await.map_err(runtime_err)?;
            if let zim_engine_application::supervision::ObservedState::Offline {
                exit_code,
                oom_killed,
            } = state
            {
                return Ok(Some((exit_code, oom_killed)));
            }
            if started.elapsed() >= deadline {
                return Ok(None);
            }
            tokio::time::sleep(EXIT_POLL_INTERVAL).await;
        }
    }

    /// Stop the workload the Minecraft way.
    ///
    /// The console-line convention: the spec's stop command is sent
    /// to the console, the process exit is awaited within the
    /// graceful deadline (the world save completes), and the
    /// runtime's graceful-then-forcible stop is the backstop for a
    /// server that ignores the command. Signal-shaped stop configs
    /// route straight to the runtime's stop.
    pub async fn stop_async(&self) -> Result<StopOutcome, IntegrationError> {
        let plan = lifecycle_plan(&self.spec, "stop")?;
        let Some(stop) = plan.stop else {
            return Err(IntegrationError::unsupported(
                "org.zaminmc.workload.minecraft",
                "stop_async",
            ));
        };
        if stop.signal {
            self.runtime
                .stop(self.stop_deadline)
                .await
                .map_err(runtime_err)?;
            return Ok(StopOutcome::Signaled);
        }
        // The console path needs a live attachment: open one (the
        // runtime tolerates a re-attach — the writer is replaced).
        // A workload that is already offline is stopped (the
        // Runtime's 404 tolerance, mirrored here).
        if let Err(err) = self.attach_console().await {
            if err_is_not_running(&err) {
                if let Ok(ObservedState::Offline { .. }) = self.runtime.state().await {
                    return Ok(StopOutcome::AlreadyOffline);
                }
            }
            return Err(err);
        }
        self.send_input_async(&stop.command).await?;
        if let Some((exit_code, oom_killed)) = self.wait_for_exit(self.stop_deadline).await? {
            return Ok(StopOutcome::Console {
                exit_code,
                oom_killed,
            });
        }
        // The backstop: graceful-then-forcible through the runtime.
        self.runtime
            .stop(self.stop_deadline)
            .await
            .map_err(runtime_err)?;
        Ok(StopOutcome::Forced)
    }

    /// Quiesce for migration: stop the workload the Minecraft way
    /// and record the checkpoint marker.
    async fn quiesce_async(&self) -> Result<MigrationCheckpoint, IntegrationError> {
        let outcome = self.stop_async().await?;
        let (exit_code, oom_killed) = match outcome {
            StopOutcome::Console {
                exit_code,
                oom_killed,
            } => (exit_code, oom_killed),
            StopOutcome::Signaled | StopOutcome::Forced => (None, false),
            StopOutcome::AlreadyOffline => (None, false),
        };
        let marker = format!(
            "quiesced:{}:exit={}:oom={}",
            chrono::Utc::now().to_rfc3339(),
            exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "none".to_owned()),
            oom_killed
        );
        Ok(MigrationCheckpoint { marker })
    }
}

/// How a stop landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// The console command was sent and the process exited cleanly
    /// within the deadline (the save completed).
    Console {
        /// The observed exit code.
        exit_code: Option<i32>,
        /// Whether the exit was OOM-caused.
        oom_killed: bool,
    },
    /// The spec's stop is signal-shaped; the runtime's graceful stop
    /// ran.
    Signaled,
    /// The console command was ignored; the runtime's
    /// graceful-then-forcible backstop ran.
    Forced,
    /// The workload was already offline when the stop arrived.
    AlreadyOffline,
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

/// Whether an integration error came from a NotRunning runtime
/// refusal.
fn err_is_not_running(error: &IntegrationError) -> bool {
    matches!(error, IntegrationError::NotRunning { .. })
}

/// Map the runtime's resource sample onto the SDK's §4 shape.
fn to_sdk_usage(usage: &RuntimeUsage) -> SdkUsage {
    // CPU normalized to one core (1.0 = one full core, §4): the
    // cumulative container share of the host's cumulative time,
    // scaled by the online-CPU count. A one-shot sample has no
    // window; the cumulative ratio is the honest portable form.
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

// ── The §4 capability implementations ───────────────────────────

impl ConsoleCapable for MinecraftWorkload {
    fn console_tail(&self, lines: usize) -> Vec<String> {
        self.console.tail(lines)
    }

    fn send_input(&self, line: &str) -> Result<(), IntegrationError> {
        // Sync bridge: see the module docs — call from a blocking
        // context (job workers / spawn_blocking), never from inside
        // the async runtime's worker threads.
        self.io.block_on(self.send_input_async(line))
    }
}

impl FilesystemCapable for MinecraftWorkload {
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

impl ExecCapable for MinecraftWorkload {
    fn exec(&self, command: SdkExecCommand) -> Result<SdkExecOutput, IntegrationError> {
        // The SDK's command form is a shell line; the runtime's is
        // argv. One-shot console-adjacent commands in the Minecraft
        // world are shell idioms (`rm -rf plugins/...`), so the
        // bridge runs the line through the container's `/bin/sh`.
        // Engine-internal callers with argv-shaped needs use
        // `exec_async` directly.
        let argv = RuntimeExecCommand {
            cmd: vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                command.command.clone(),
            ],
            env: Vec::new(),
            user: None,
            working_dir: Some(self.cfg.container_data_dir.to_string_lossy().into_owned()),
            timeout: Some(command.timeout),
        };
        let output = self.io.block_on(self.exec_async(argv))?;
        Ok(SdkExecOutput {
            exit_code: output.exit_code.and_then(|code| i32::try_from(code).ok()),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

impl MetricsCapable for MinecraftWorkload {
    fn metrics(&self) -> Result<SdkUsage, IntegrationError> {
        self.io.block_on(self.metrics_async())
    }
}

impl Migratable for MinecraftWorkload {
    fn migration_plan(&self) -> Result<MigrationPlan, IntegrationError> {
        // The roadmap's M17 contract: the file-copy migration plan
        // reports requires_quiesce = true for safety — the world
        // save must complete before the bytes ship.
        let data_bytes = self.data_dir_bytes();
        let ship_seconds = data_bytes as f64 / MIGRATION_BYTES_PER_SECOND;
        let estimate = self.stop_deadline
            + Duration::from_secs(ship_seconds.ceil() as u64)
            + Duration::from_secs(5); // the relaunch allowance
        Ok(MigrationPlan {
            requires_quiesce: true,
            estimated_downtime: estimate,
        })
    }

    fn prepare_for_migration(&self) -> Result<MigrationCheckpoint, IntegrationError> {
        self.io.block_on(self.quiesce_async())
    }

    fn finalize_migration(&self) -> Result<(), IntegrationError> {
        // The transfer landed: relaunch from the carried
        // configuration on the destination side.
        self.io.block_on(self.start_async())
    }

    fn abort_migration(&self, _checkpoint: MigrationCheckpoint) -> Result<(), IntegrationError> {
        // Roll back to the checkpoint: the local bytes were never
        // disturbed by a quiesce, so the recovery is the relaunch.
        self.io.block_on(self.start_async())
    }
}

impl MinecraftWorkload {
    /// The data directory's current size in bytes (the migration
    /// estimate's input; missing trees are zero-sized).
    fn data_dir_bytes(&self) -> u64 {
        fn walk(dir: &std::path::Path) -> u64 {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return 0;
            };
            let mut total = 0;
            for entry in entries.flatten() {
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                if meta.is_dir() {
                    total += walk(&entry.path());
                } else {
                    total += meta.len();
                }
            }
            total
        }
        walk(&self.cfg.data_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sdk_usage_mapping_normalizes_cpu_and_sums_network() {
        let usage = RuntimeUsage {
            cpu_usage_total_ns: Some(100_000_000_000),
            system_cpu_total_ns: Some(800_000_000_000),
            online_cpus: Some(4),
            memory_usage_bytes: Some(512 * 1024 * 1024),
            memory_limit_bytes: Some(1024 * 1024 * 1024),
            pids_current: Some(37),
            network_rx_bytes: Some(1_000),
            network_tx_bytes: Some(250),
        };
        let mapped = to_sdk_usage(&usage);
        // 100/800 of cumulative time * 4 cores = 0.5 cores.
        assert!((mapped.cpu - 0.5).abs() < 1e-6, "cpu = {}", mapped.cpu);
        assert_eq!(mapped.memory_bytes, 512 * 1024 * 1024);
        assert_eq!(mapped.network_bytes, 1_250);

        // Degenerate forms collapse to zeros, never NaN.
        let degenerate = RuntimeUsage::default();
        let mapped = to_sdk_usage(&degenerate);
        assert_eq!(mapped.cpu, 0.0);
        assert_eq!(mapped.memory_bytes, 0);
        assert_eq!(mapped.network_bytes, 0);

        let zero_system = RuntimeUsage {
            cpu_usage_total_ns: Some(1),
            system_cpu_total_ns: Some(0),
            online_cpus: Some(4),
            ..RuntimeUsage::default()
        };
        assert_eq!(to_sdk_usage(&zero_system).cpu, 0.0);
    }

    #[test]
    fn the_archive_format_mapping_is_total() {
        assert_eq!(
            to_files_format(SdkFormat::Tar),
            zim_engine_files::ArchiveFormat::Tar
        );
        assert_eq!(
            to_files_format(SdkFormat::TarGz),
            zim_engine_files::ArchiveFormat::TarGz
        );
        assert_eq!(
            to_files_format(SdkFormat::Zip),
            zim_engine_files::ArchiveFormat::Zip
        );
    }

    #[test]
    fn runtime_errors_map_onto_the_integration_vocabulary() {
        let err = runtime_err(RuntimeError::NotRunning("gone".to_owned()));
        assert!(matches!(err, IntegrationError::NotRunning { .. }));
        let err = runtime_err(RuntimeError::Backend("boom".to_owned()));
        assert!(matches!(err, IntegrationError::Runtime(_)));
        assert!(err_is_not_running(&runtime_err(RuntimeError::NotRunning(
            "gone".to_owned()
        ))));
        assert!(!err_is_not_running(&runtime_err(RuntimeError::Backend(
            "boom".to_owned()
        ))));
    }
}
