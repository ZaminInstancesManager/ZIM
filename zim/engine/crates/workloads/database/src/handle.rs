//! The per-workload Database handle (M18): the §4 capability
//! implementations over a live [`Runtime`].
//!
//! [`DatabaseWorkload`] fuses one workload's runtime handle, its
//! instance configuration, its file root, and its spec into the
//! typed surface the SDK's capability traits describe:
//!
//! - [`ReadyProbeCapable`] — the per-engine probe (`pg_isready`,
//!   `mysqladmin ping` / `mariadb-admin ping`, `redis-cli ping`)
//!   executed in the workload's container and classified per
//!   [`crate::probe`];
//! - [`FilesystemCapable`] — the Wings file API over the instance's
//!   data directory;
//! - [`SnapshotCapable`] — the §M18 consistency story: quiesce
//!   (`CHECKPOINT` / `FLUSH TABLES WITH READ LOCK` / `BGSAVE`),
//!   archive, and the restore path (stop → clear → extract →
//!   start → wait for readiness);
//! - [`NetworkCapable`] — the multi-endpoint surface
//!   (read/write/replication logical names over the placed
//!   bindings; live re-plumbing applies at the next start, the
//!   Docker constraint);
//! - [`MetricsCapable`] — the Docker-stats resource sample;
//! - [`Migratable`] — the §M18 multi-step family: the
//!   engine-marker checkpoint, the quiesce, the relaunch on
//!   finalize/abort.
//!
//! # The sync↔async bridge
//!
//! Same shape as the Minecraft handle (M17): the SDK's capability
//! traits are synchronous; the engine's [`Runtime`] is async. The
//! handle carries the engine runtime's [`tokio::runtime::Handle`]
//! and the trait implementations `block_on` their async twins
//! through it. **Calling the sync trait methods from inside an
//! async context on the same runtime deadlocks** — they are for
//! the job-executor's worker threads and `spawn_blocking`
//! contexts. Async callers use the inherent `*_async` methods.
//!
//! # Configuration mutability
//!
//! Unlike the Minecraft handle (an immutable `Arc<InstanceConfig>`),
//! the database handle carries `Arc<RwLock<InstanceConfig>>`:
//! [`NetworkCapable`]'s expose/unexpose mutate the carried
//! configuration, which the next
//! [`start`][DatabaseWorkload::start_async] applies (Docker cannot
//! re-plumb a live container's port bindings — recreate-on-start
//! is the engine's own convention, so the mutation is naturally
//! start-aligned).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use zim_engine_application::supervision::{
    ExecCommand as RuntimeExecCommand, ExecOutput as RuntimeExecOutput, InstanceConfig,
    ObservedState, ResourceUsage as RuntimeUsage, Runtime, RuntimeError,
};
use zim_engine_files::{FileError, Filesystem};
use zim_engine_workloads_sdk::errors::IntegrationError;
use zim_engine_workloads_sdk::traits::{
    ArchiveFormat as SdkFormat, ArchiveRef, Endpoint, ExecCapable, FilesystemCapable,
    MetricsCapable, Migratable, MigrationCheckpoint, MigrationPlan, NetworkCapable, PortSpec,
    ReadyProbeCapable, ResourceUsage as SdkUsage, SnapshotCapable, SnapshotRef,
};

use crate::engine::DatabaseEngine;
use crate::lifecycle::DEFAULT_STOP_DEADLINE;
use crate::probe::classify_probe;
use crate::spec::DatabaseSpec;

/// How often readiness polls.
const READINESS_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How often the one-shot containers' exit is polled.
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// How often the Redis BGSAVE completion poll runs.
const BGSAVE_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The migration throughput assumption for the downtime estimate
/// (bytes/second; deliberately conservative so the surfaced window
/// over-promises time rather than under-promising it — the same
/// posture as the Minecraft handle's estimate).
const MIGRATION_BYTES_PER_SECOND: f64 = 50.0 * 1024.0 * 1024.0;

/// The allowance a restored engine needs to replay its recovery
/// log and reach readiness (the restore path's tail, folded into
/// the migration downtime estimate).
const RECOVERY_REPLAY_ALLOWANCE: Duration = Duration::from_secs(30);

/// The first host port the handle's expose path allocates from
/// (above the ephemeral range's usual users; tests bind unique
/// ports below it through their own allocation).
const FIRST_EXPOSED_HOST_PORT: u16 = 24500;

/// The per-workload Database capability handle.
#[derive(Clone)]
pub struct DatabaseWorkload {
    runtime: Arc<dyn Runtime>,
    cfg: Arc<RwLock<InstanceConfig>>,
    io: tokio::runtime::Handle,
    files: Filesystem,
    spec: DatabaseSpec,
    next_host_port: Arc<AtomicU16>,
    stop_deadline: Duration,
    probe_timeout: Duration,
    restore_ready_timeout: Duration,
}

impl DatabaseWorkload {
    /// Build the handle for one placed workload.
    ///
    /// The expose-port allocator starts above the configuration's
    /// highest bound host port (or the default floor), so
    /// `expose_port` never collides with the placed allocations.
    pub fn new(
        runtime: Arc<dyn Runtime>,
        cfg: Arc<InstanceConfig>,
        io: tokio::runtime::Handle,
        spec: DatabaseSpec,
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
            restore_ready_timeout: Duration::from_secs(180),
        }
    }

    /// Override the graceful-stop deadline.
    pub fn with_stop_deadline(mut self, deadline: Duration) -> Self {
        self.stop_deadline = deadline;
        self
    }

    /// Override the per-probe exec timeout.
    pub fn with_probe_timeout(mut self, timeout: Duration) -> Self {
        self.probe_timeout = timeout;
        self
    }

    /// Override the restore path's readiness deadline (how long a
    /// restored engine may take to replay its recovery log and
    /// answer its probe).
    pub fn with_restore_ready_timeout(mut self, timeout: Duration) -> Self {
        self.restore_ready_timeout = timeout;
        self
    }

    /// The handle's spec.
    pub fn spec(&self) -> &DatabaseSpec {
        &self.spec
    }

    /// The handle's file API.
    pub fn files(&self) -> &Filesystem {
        &self.files
    }

    /// A snapshot of the carried instance configuration (the
    /// expose/unexpose surface's current state).
    pub fn current_config(&self) -> InstanceConfig {
        self.cfg
            .read()
            .expect("the instance-config lock is only held briefly")
            .clone()
    }

    // ── The async twins ─────────────────────────────────────────

    /// (Re)create and start the workload's container from the
    /// carried configuration (the runtime's Wings-parity
    /// destroy-and-recreate; applies any expose/unexpose edits).
    pub async fn start_async(&self) -> Result<(), IntegrationError> {
        let cfg = self
            .cfg
            .read()
            .expect("the instance-config lock is only held briefly")
            .clone();
        self.runtime.start(&cfg).await.map_err(runtime_err)
    }

    /// Stop the workload the database way: the runtime's
    /// graceful-then-forcible signal stop, which every M18
    /// engine's entrypoint translates into its orderly shutdown
    /// (there is no console — the signal path *is* the stop
    /// convention; see [`crate::lifecycle`]).
    pub async fn stop_async(&self) -> Result<(), IntegrationError> {
        self.runtime
            .stop(self.stop_deadline)
            .await
            .map_err(runtime_err)
    }

    /// Run the engine's readiness probe once and classify it.
    pub async fn probe_async(
        &self,
    ) -> Result<zim_engine_workloads_sdk::traits::Readiness, IntegrationError> {
        let creds = self.spec.resolved_credentials();
        let argv = self
            .spec
            .engine
            .probe_argv(&creds, self.spec.primary_container_port());
        let output = self.exec_argv_async(&argv, self.probe_timeout).await;
        match output {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                Ok(classify_probe(
                    self.spec.engine,
                    output.exit_code.and_then(|code| i32::try_from(code).ok()),
                    &stdout,
                    &stderr,
                ))
            }
            // A stopped database is certainly not ready (the
            // process-level reason — readiness distinct from
            // process presence, and the probe has nowhere to run).
            Err(err) if err_is_not_running(&err) => {
                Ok(zim_engine_workloads_sdk::traits::Readiness::NotReady {
                    reason: "the workload is not running".to_owned(),
                })
            }
            // Probe infrastructure failure: the probe could not
            // determine readiness.
            Err(_) => Ok(zim_engine_workloads_sdk::traits::Readiness::Unknown),
        }
    }

    /// Poll the readiness probe until it answers `Ready` or the
    /// deadline passes (the §4 `wait_until_ready` semantics).
    pub async fn wait_until_ready_async(&self, deadline: Duration) -> Result<(), IntegrationError> {
        let started = tokio::time::Instant::now();
        loop {
            if matches!(
                self.probe_async().await?,
                zim_engine_workloads_sdk::traits::Readiness::Ready
            ) {
                return Ok(());
            }
            if started.elapsed() >= deadline {
                return Err(IntegrationError::Limit(format!(
                    "the {} workload did not become ready within {:?}",
                    self.spec.engine, deadline
                )));
            }
            tokio::time::sleep(READINESS_POLL_INTERVAL).await;
        }
    }

    /// Evaluate one statement through the engine's own client
    /// (the parity suite's query path and the engine's marker
    /// extraction).
    pub async fn eval_async(&self, statement: &str) -> Result<RuntimeExecOutput, IntegrationError> {
        let creds = self.spec.resolved_credentials();
        let argv = self.spec.engine.eval_argv(&creds, statement);
        self.exec_argv_async(&argv, Duration::from_secs(30)).await
    }

    /// The engine's point-in-time marker (WAL LSN / LASTSAVE /
    /// server time), trimmed to its last non-empty line.
    pub async fn marker_async(&self) -> Result<String, IntegrationError> {
        let creds = self.spec.resolved_credentials();
        let argv = self.spec.engine.marker_argv(&creds);
        let output = self.exec_argv_async(&argv, Duration::from_secs(30)).await?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let exit = output.exit_code.unwrap_or(-1);
        if exit != 0 {
            return Err(IntegrationError::Runtime(format!(
                "the {} marker statement failed (exit {exit}): {}",
                self.spec.engine,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let line = stdout
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .trim()
            .to_owned();
        if line.is_empty() {
            return Err(IntegrationError::Runtime(format!(
                "the {} marker statement answered nothing",
                self.spec.engine
            )));
        }
        Ok(line)
    }

    /// One resource-usage sample (the Docker stats capability).
    pub async fn metrics_async(&self) -> Result<SdkUsage, IntegrationError> {
        let usage = self.runtime.stats().await.map_err(runtime_err)?;
        Ok(to_sdk_usage(&usage))
    }

    /// Run one argv inside the workload's container, bounded by
    /// `timeout` (the probes' and engines' own clients, engine
    /// environment attached).
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

    // ── The snapshot family ─────────────────────────────────────

    /// The snapshot store's data-directory-relative tree (the
    /// engine table's own constant, re-declared for the archive
    /// path helper).
    const SNAPSHOT_DIR: &'static str = "snapshots";

    /// How long one container-side archive (the snapshot exec or
    /// the one-shot archive container) may run.
    const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(300);

    /// How long the one-shot containers (archive, restore) may take
    /// to run to exit.
    const ONE_SHOT_DEADLINE: Duration = Duration::from_secs(120);

    /// The archive path (data-directory-relative) one label maps
    /// onto.
    fn archive_path(label: &str) -> String {
        format!("{}/{label}.tar.gz", Self::SNAPSHOT_DIR)
    }

    /// Validate a snapshot label: the archive path is built from
    /// it, so it carries the same rules the file API's path
    /// grammar enforces (one path component, no traversal).
    fn validate_label(label: &str) -> Result<(), IntegrationError> {
        let clean = !label.is_empty()
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            && !label.starts_with('.')
            && !label.contains("..")
            && !label.ends_with(".tar.gz");
        if clean {
            Ok(())
        } else {
            Err(IntegrationError::Limit(format!(
                "the snapshot label {label:?} is not a plain name \
                 (alphanumerics, '-', '_', '.'; no path shapes)"
            )))
        }
    }

    /// Take one snapshot: quiesce the engine its own way, then
    /// archive the data directory container-side (§M18's
    /// `SnapshotCapable`).
    ///
    /// The quiesce-then-archive sequence per engine:
    ///
    /// - **Postgres** — `CHECKPOINT` (flush + checkpoint record),
    ///   then the container-side `tar` of the data directory minus
    ///   the snapshot store. The image is crash-consistent; the WAL
    ///   inside it replays on restore.
    /// - **MySQL / MariaDB** — one exec whose client session holds
    ///   `FLUSH TABLES WITH READ LOCK` across a container-side
    ///   `tar` of the data directory (the `SYSTEM` trick). Writes
    ///   are blocked for the archive's whole duration; the
    ///   session's nonzero exit is tolerated while the archive
    ///   itself verifies (tar's "file changed as we read it"
    ///   warning on a flushing InnoDB is expected).
    /// - **Redis** — `BGSAVE`, polled until the RDB write
    ///   completes, then the container-side `tar`.
    ///
    /// A **stopped** workload snapshots through the one-shot
    /// container: the stop *was* the quiesce (Postgres's fast
    /// shutdown checkpoints and exits; Redis's shutdown-save
    /// persists), so the archive container runs the same shell line
    /// without any engine quiesce — the migration flow's
    /// snapshot-after-prepare rides exactly this path.
    ///
    /// The verification is the shell line's own trailing `stat`
    /// (the archive's byte size in the exec's output) — the
    /// host-side actor cannot stat through the engines'
    /// ownership-taken data directories, so the in-container
    /// `stat` is the honest contract.
    pub async fn snapshot_async(&self, label: &str) -> Result<SnapshotRef, IntegrationError> {
        Self::validate_label(label)?;
        let creds = self.spec.resolved_credentials();
        let archive_path = Self::archive_path(label);
        let data_dir = self
            .cfg
            .read()
            .expect("the instance-config lock is only held briefly")
            .container_data_dir
            .to_string_lossy()
            .into_owned();

        let running = self
            .runtime
            .state()
            .await
            .map_err(runtime_err)?
            .is_running();
        if running {
            if !self.spec.engine.uses_lock_held_snapshot() {
                self.quiesce_async(&creds).await?;
            }
            let argv = self
                .spec
                .engine
                .snapshot_exec_argv(&creds, &data_dir, &archive_path);
            let output = self.exec_argv_async(&argv, Self::ARCHIVE_TIMEOUT).await?;
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            let exit = output.exit_code.unwrap_or(-1);
            // The stat line at the output's tail is the success
            // contract (a positive byte size); the exit code is
            // strict for the direct-shell engines and advisory for
            // the lock-held family (a warning-exiting tar inside
            // the client session is tolerated while the archive
            // verifies).
            let size = stdout
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .and_then(|line| line.trim().parse::<u64>().ok())
                .unwrap_or(0);
            let tolerated_exit = self.spec.engine.uses_lock_held_snapshot();
            if size == 0 || (!tolerated_exit && exit != 0) {
                return Err(IntegrationError::Runtime(format!(
                    "the {} snapshot archive did not verify (exit {exit}, \
                     size {size}): {}{}",
                    self.spec.engine,
                    stdout.trim(),
                    stderr.trim()
                )));
            }
            if tolerated_exit && exit != 0 {
                eprintln!(
                    "zim: the {} lock-held snapshot's client exited {exit} \
                     (tolerated; the archive verified at {size} bytes): {}",
                    self.spec.engine,
                    stderr.trim()
                );
            }
        } else {
            // The stopped path: a one-shot archive container (the
            // stop was the quiesce).
            let script = self.spec.engine.archive_shell(&data_dir, &archive_path);
            self.run_one_shot_container(&script, Self::ONE_SHOT_DEADLINE)
                .await?;
        }
        Ok(SnapshotRef {
            label: label.to_owned(),
        })
    }

    /// The running engines' quiesce step (Postgres's CHECKPOINT;
    /// Redis's BGSAVE polled to completion).
    async fn quiesce_async(
        &self,
        creds: &crate::spec::DatabaseCredentials,
    ) -> Result<(), IntegrationError> {
        match self.spec.engine {
            DatabaseEngine::Postgres => {
                let argv = self
                    .spec
                    .engine
                    .quiesce_argv(creds)
                    .expect("postgres quiesce argv");
                let output = self.exec_argv_async(&argv, Duration::from_secs(60)).await?;
                let exit = output.exit_code.unwrap_or(-1);
                if exit != 0 {
                    return Err(IntegrationError::Runtime(format!(
                        "CHECKPOINT failed (exit {exit}): {}{}",
                        String::from_utf8_lossy(&output.stdout).trim(),
                        String::from_utf8_lossy(&output.stderr).trim()
                    )));
                }
                Ok(())
            }
            DatabaseEngine::Redis => {
                let argv = self
                    .spec
                    .engine
                    .quiesce_argv(creds)
                    .expect("redis quiesce argv");
                let output = self.exec_argv_async(&argv, Duration::from_secs(30)).await?;
                let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                let exit = output.exit_code.unwrap_or(-1);
                if exit != 0 || !stdout.contains("Background saving started") {
                    return Err(IntegrationError::Runtime(format!(
                        "BGSAVE did not start (exit {exit}): {}{}",
                        stdout.trim(),
                        String::from_utf8_lossy(&output.stderr).trim()
                    )));
                }
                // Poll until the RDB write completes.
                let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
                loop {
                    if self.rdb_save_complete_async(creds).await? {
                        return Ok(());
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Err(IntegrationError::Limit(
                            "the Redis BGSAVE did not complete within 60s".to_owned(),
                        ));
                    }
                    tokio::time::sleep(BGSAVE_POLL_INTERVAL).await;
                }
            }
            DatabaseEngine::MySql | DatabaseEngine::MariaDb => Err(IntegrationError::unsupported(
                "org.zaminmc.workload.database",
                "quiesce_async",
            )),
        }
    }

    /// Whether Redis's background save has completed (`INFO
    /// persistence`'s `rdb_bgsave_in_progress:0`).
    async fn rdb_save_complete_async(
        &self,
        creds: &crate::spec::DatabaseCredentials,
    ) -> Result<bool, IntegrationError> {
        let argv = self.spec.engine.eval_argv(creds, "INFO persistence");
        let output = self.exec_argv_async(&argv, Duration::from_secs(15)).await?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        Ok(stdout
            .lines()
            .any(|line| line.trim().starts_with("rdb_bgsave_in_progress:0")))
    }

    /// Restore one snapshot (§M18): stop the workload, run the
    /// one-shot restore container (clear the data directory except
    /// the snapshot store, extract the archive), and relaunch —
    /// ready when the engine's probe says so (the recovery log
    /// replays on the way up).
    ///
    /// The one-shot container is the engine's own entrypoint
    /// running a shell command instead of the server binary: the
    /// entrypoints pass non-server commands straight to `exec` as
    /// the root entrypoint user, which is exactly the privilege the
    /// data-directory rewrite needs (the engines' entrypoints
    /// `chown` the restored files on the next real start).
    pub async fn restore_snapshot_async(
        &self,
        snapshot: &SnapshotRef,
    ) -> Result<(), IntegrationError> {
        Self::validate_label(&snapshot.label)?;
        let archive_path = Self::archive_path(&snapshot.label);
        let data_dir = self
            .cfg
            .read()
            .expect("the instance-config lock is only held briefly")
            .container_data_dir
            .to_string_lossy()
            .into_owned();

        // 1. Stop (an already-offline workload is fine — the
        //    runtime's 404 tolerance).
        self.stop_async().await?;

        // 2. The one-shot restore container (clear + extract,
        //    exit 0 required).
        let script = self.spec.engine.restore_script(&data_dir, &archive_path);
        self.run_one_shot_container(&script, Self::ONE_SHOT_DEADLINE)
            .await?;

        // 3. Relaunch and wait for readiness (the engine's own
        //    recovery replay completes the restore).
        self.start_async().await?;
        self.wait_until_ready_async(self.restore_ready_timeout)
            .await
    }

    /// Run one shell script as the workload's container, to exit:
    /// swap the carried command for `/bin/sh -c script`, start
    /// (the runtime's recreate-on-start applies it), await the
    /// exit, and restore the real command — including on failure.
    async fn run_one_shot_container(
        &self,
        script: &str,
        deadline: Duration,
    ) -> Result<(), IntegrationError> {
        let restore_command: Vec<String> = {
            let mut cfg = self
                .cfg
                .write()
                .expect("the instance-config lock is only held briefly");
            std::mem::replace(
                &mut cfg.command,
                vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()],
            )
        };

        let outcome = async {
            self.start_async().await?;
            let started = tokio::time::Instant::now();
            loop {
                if let ObservedState::Offline { exit_code, .. } =
                    self.runtime.state().await.map_err(runtime_err)?
                {
                    if exit_code == Some(0) {
                        return Ok(());
                    }
                    return Err(IntegrationError::Runtime(format!(
                        "the one-shot container exited with {exit_code:?} \
                         (expected 0)"
                    )));
                }
                if started.elapsed() >= deadline {
                    return Err(IntegrationError::Limit(format!(
                        "the one-shot container did not exit within {deadline:?}"
                    )));
                }
                tokio::time::sleep(EXIT_POLL_INTERVAL).await;
            }
        }
        .await;

        // The carried command always returns to the server's real
        // argv (a failed one-shot must not poison the next start).
        self.cfg
            .write()
            .expect("the instance-config lock is only held briefly")
            .command = restore_command;

        outcome
    }

    // ── The migration family ────────────────────────────────────

    /// Quiesce for migration: capture the engine's point-in-time
    /// marker, then take the engine's orderly shutdown.
    async fn prepare_migration_async(&self) -> Result<MigrationCheckpoint, IntegrationError> {
        let marker = self.marker_async().await?;
        self.stop_async().await?;
        Ok(MigrationCheckpoint {
            marker: format!(
                "{}:{}:quiesced={}",
                self.spec.engine,
                marker,
                chrono::Utc::now().to_rfc3339()
            ),
        })
    }

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
        walk(self.files.root())
    }
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

/// Map the runtime's resource sample onto the SDK's §4 shape
/// (the same normalization the Minecraft handle applies: CPU
/// normalized to one core, network = rx+tx).
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

/// The protocol word the SDK's port spec carries, mapped onto the
/// supervision vocabulary's.
fn port_protocol(protocol: &str) -> zim_engine_application::supervision::PortProtocol {
    if protocol.eq_ignore_ascii_case("udp") {
        zim_engine_application::supervision::PortProtocol::Udp
    } else {
        zim_engine_application::supervision::PortProtocol::Tcp
    }
}

// ── The §4 capability implementations ───────────────────────────

impl ReadyProbeCapable for DatabaseWorkload {
    fn readiness(&self) -> Result<zim_engine_workloads_sdk::traits::Readiness, IntegrationError> {
        // Sync bridge: see the module docs — call from a blocking
        // context (job workers / spawn_blocking).
        self.io.block_on(self.probe_async())
    }

    fn wait_until_ready(&self, deadline: Duration) -> Result<(), IntegrationError> {
        self.io.block_on(self.wait_until_ready_async(deadline))
    }
}

impl FilesystemCapable for DatabaseWorkload {
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

impl ExecCapable for DatabaseWorkload {
    fn exec(
        &self,
        command: zim_engine_workloads_sdk::traits::ExecCommand,
    ) -> Result<zim_engine_workloads_sdk::traits::ExecOutput, IntegrationError> {
        // The SDK's command form is a shell line; the runtime's is
        // argv. Database-side one-shots are engine client calls
        // (`psql -c ...`, `redis-cli ...`) — the bridge runs the
        // line through the container's shell with the engine's
        // environment attached.
        let argv = RuntimeExecCommand {
            cmd: vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                command.command.clone(),
            ],
            env: Vec::new(),
            user: None,
            working_dir: None,
            timeout: Some(command.timeout),
        };
        let output = self
            .io
            .block_on(self.runtime.exec(&argv))
            .map_err(runtime_err)?;
        Ok(zim_engine_workloads_sdk::traits::ExecOutput {
            exit_code: output.exit_code.and_then(|code| i32::try_from(code).ok()),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

impl MetricsCapable for DatabaseWorkload {
    fn metrics(&self) -> Result<SdkUsage, IntegrationError> {
        self.io.block_on(self.metrics_async())
    }
}

impl SnapshotCapable for DatabaseWorkload {
    fn snapshot(&self, label: &str) -> Result<SnapshotRef, IntegrationError> {
        self.io.block_on(self.snapshot_async(label))
    }

    fn restore_snapshot(&self, snapshot: &SnapshotRef) -> Result<(), IntegrationError> {
        self.io.block_on(self.restore_snapshot_async(snapshot))
    }
}

impl NetworkCapable for DatabaseWorkload {
    fn endpoints(&self) -> Result<Vec<Endpoint>, IntegrationError> {
        let cfg = self
            .cfg
            .read()
            .expect("the instance-config lock is only held briefly")
            .clone();
        // Every declared logical endpoint, resolved to its placed
        // host port (an unplaced endpoint — no binding for its
        // container port — is simply not reported yet).
        let placed: Vec<Endpoint> = self
            .spec
            .effective_endpoints()
            .into_iter()
            .filter_map(|endpoint| {
                let binding = cfg.port_bindings.iter().find(|binding| {
                    binding.container_port == endpoint.container_port
                        && matches!(
                            binding.protocol,
                            zim_engine_application::supervision::PortProtocol::Tcp
                        )
                })?;
                Some(Endpoint {
                    name: endpoint.name,
                    port: binding.host_port,
                })
            })
            .collect();
        // Plus any bound-but-undeclared extra ports the expose
        // path added (they carry their own logical names).
        let mut all = placed;
        for binding in &cfg.port_bindings {
            if matches!(
                binding.protocol,
                zim_engine_application::supervision::PortProtocol::Udp
            ) {
                continue;
            }
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
        // An existing binding for the same (container port,
        // protocol) is the same exposure — report it (idempotent).
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
        // The logical name resolves to its container port through
        // the spec's declared endpoints (or the expose-added
        // bindings' own ports).
        let spec_ports: Vec<u16> = self
            .spec
            .effective_endpoints()
            .iter()
            .filter(|declared| declared.name == endpoint.name)
            .map(|declared| declared.container_port)
            .collect();
        let mut cfg = self
            .cfg
            .write()
            .expect("the instance-config lock is only held briefly");
        let container_ports: Vec<u16> = if !spec_ports.is_empty() {
            spec_ports
        } else {
            // Not a declared name: resolve through the placed
            // binding with that host port.
            cfg.port_bindings
                .iter()
                .filter(|binding| binding.host_port == endpoint.port)
                .map(|binding| binding.container_port)
                .collect()
        };
        if container_ports.is_empty() {
            // Nothing bound under that name/port: already
            // unexposed.
            return Ok(());
        }
        // A container port still referenced by another declared
        // endpoint (the primary/replication same-port shape) keeps
        // its binding.
        let still_referenced: Vec<u16> = self
            .spec
            .effective_endpoints()
            .iter()
            .filter(|declared| declared.name != endpoint.name)
            .map(|declared| declared.container_port)
            .collect();
        cfg.port_bindings.retain(|binding| {
            !container_ports.contains(&binding.container_port)
                || still_referenced.contains(&binding.container_port)
        });
        Ok(())
    }
}

impl Migratable for DatabaseWorkload {
    fn migration_plan(&self) -> Result<MigrationPlan, IntegrationError> {
        // The §M18 multi-step family — quiesce, checkpoint WAL,
        // snapshot, ship, restore, catch up — is a stop-the-world
        // sequence for the M18 engines: requires_quiesce is true.
        let data_bytes = self.data_dir_bytes();
        let ship_seconds = data_bytes as f64 / MIGRATION_BYTES_PER_SECOND;
        let estimate = self.stop_deadline
            + Duration::from_secs(ship_seconds.ceil() as u64)
            + RECOVERY_REPLAY_ALLOWANCE;
        Ok(MigrationPlan {
            requires_quiesce: true,
            estimated_downtime: estimate,
        })
    }

    fn prepare_for_migration(&self) -> Result<MigrationCheckpoint, IntegrationError> {
        self.io.block_on(self.prepare_migration_async())
    }

    fn finalize_migration(&self) -> Result<(), IntegrationError> {
        // The transfer landed: relaunch from the carried
        // configuration on the destination side (the recovery
        // replay catches up to the checkpoint's marker).
        self.io.block_on(self.start_async())
    }

    fn abort_migration(&self, _checkpoint: MigrationCheckpoint) -> Result<(), IntegrationError> {
        // Roll back to the checkpoint: the local bytes were never
        // disturbed by a quiesce, so the recovery is the relaunch.
        self.io.block_on(self.start_async())
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
        assert!((mapped.cpu - 0.5).abs() < 1e-6, "cpu = {}", mapped.cpu);
        assert_eq!(mapped.memory_bytes, 512 * 1024 * 1024);
        assert_eq!(mapped.network_bytes, 1_250);

        let degenerate = RuntimeUsage::default();
        let mapped = to_sdk_usage(&degenerate);
        assert_eq!(mapped.cpu, 0.0);
        assert_eq!(mapped.memory_bytes, 0);
        assert_eq!(mapped.network_bytes, 0);
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
    fn the_protocol_words_map() {
        use zim_engine_application::supervision::PortProtocol;
        assert_eq!(port_protocol("tcp"), PortProtocol::Tcp);
        assert_eq!(port_protocol("TCP"), PortProtocol::Tcp);
        assert_eq!(port_protocol("udp"), PortProtocol::Udp);
        assert_eq!(port_protocol("UDP"), PortProtocol::Udp);
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

    #[test]
    fn the_snapshot_labels_are_validated() {
        for good in ["pre-migrate", "nightly_2026_10_14", "v1.2"] {
            DatabaseWorkload::validate_label(good).unwrap_or_else(|e| panic!("{good}: {e}"));
        }
        for bad in [
            "",
            ".",
            "..",
            "a/b",
            "a b",
            "pre.tar.gz",
            ".hidden",
            "..dots..",
        ] {
            assert!(
                DatabaseWorkload::validate_label(bad).is_err(),
                "{bad:?} must be refused"
            );
        }
        // The archive path is a plain two-segment form.
        assert_eq!(
            DatabaseWorkload::archive_path("pre-migrate"),
            "snapshots/pre-migrate.tar.gz"
        );
    }
}
