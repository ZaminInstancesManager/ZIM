//! The per-workload DevProcess handle (M19): the §4 capability
//! implementations over a live [`Runtime`].
//!
//! [`DevProcessWorkload`] fuses one workload's runtime handle,
//! its instance configuration, its file root, and its spec into
//! the typed surface the SDK's capability traits describe:
//!
//! - [`ConsoleCapable`] — attach the console pump, tail the
//!   retained window, send input lines (the dev loop's own
//!   stdin surface — a REPL, a prompt, a debugger);
//! - [`MetricsCapable`] — the Docker-stats resource sample;
//! - the **watch-restart supervision** — the §M19 headline:
//!   [`supervise`][Self::supervise] spawns the background task
//!   that consumes the file watcher's restart requests and
//!   performs the restart (graceful stop, relaunch, the console
//!   generation's reset, the restart counter). The §6
//!   `Watching` extended state surfaces through
//!   [`watch_state`][Self::watch_state].
//!
//! # The sync↔async bridge
//!
//! Same shape as the Minecraft and Database handles (M17/M18):
//! the SDK's capability traits are synchronous; the engine's
//! [`Runtime`] is async. The handle carries the engine runtime's
//! [`tokio::runtime::Handle`] and the trait implementations
//! `block_on` their async twins through it. **Calling the sync
//! trait methods from inside an async context on the same
//! runtime deadlocks** — they are for the job-executor's worker
//! threads and `spawn_blocking` contexts. Async callers use the
//! inherent `*_async` methods.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zim_engine_application::supervision::{
    InstanceConfig, ObservedState, ResourceUsage as RuntimeUsage, Runtime, RuntimeError,
};
use zim_engine_workloads_sdk::errors::IntegrationError;
use zim_engine_workloads_sdk::traits::{
    ConsoleCapable, MetricsCapable, ResourceUsage as SdkUsage,
};

use crate::console::ConsoleBuffer;
use crate::lifecycle::DEFAULT_STOP_DEADLINE;
use crate::spec::DevProcessSpec;
use crate::watch::{FileWatcher, WatchState};

/// How often the supervisor task polls the watcher's request
/// channel.
const SUPERVISOR_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The per-workload DevProcess capability handle.
///
/// Clone-cheap (every field is an `Arc`, a `Handle`, or a `Copy`)
/// so blocking-context callers can move a clone into
/// `spawn_blocking`.
#[derive(Clone)]
pub struct DevProcessWorkload {
    runtime: Arc<dyn Runtime>,
    cfg: Arc<InstanceConfig>,
    io: tokio::runtime::Handle,
    spec: DevProcessSpec,
    console: Arc<ConsoleBuffer>,
    /// Whether a console pump is live for the current run (attach
    /// idempotence — the same posture the Minecraft handle takes).
    attached: Arc<AtomicBool>,
    /// The file watcher (present only while a watching policy
    /// supervises).
    watcher: Arc<Mutex<Option<FileWatcher>>>,
    /// How many restarts the supervision has performed (the
    /// watch-restart family's counter — distinct from the
    /// engine's crash counter, which does not count them).
    restarts: Arc<AtomicU32>,
    stop_deadline: Duration,
}

impl DevProcessWorkload {
    /// Build the handle for one placed workload.
    pub fn new(
        runtime: Arc<dyn Runtime>,
        cfg: Arc<InstanceConfig>,
        io: tokio::runtime::Handle,
        spec: DevProcessSpec,
    ) -> Self {
        Self {
            console: Arc::new(ConsoleBuffer::new()),
            spec,
            runtime,
            cfg,
            io,
            attached: Arc::new(AtomicBool::new(false)),
            watcher: Arc::new(Mutex::new(None)),
            restarts: Arc::new(AtomicU32::new(0)),
            stop_deadline: DEFAULT_STOP_DEADLINE,
        }
    }

    /// Override the graceful-stop deadline.
    pub fn with_stop_deadline(mut self, deadline: Duration) -> Self {
        self.stop_deadline = deadline;
        self
    }

    /// The handle's spec.
    pub fn spec(&self) -> &DevProcessSpec {
        &self.spec
    }

    /// How many restarts the supervision has performed.
    pub fn restarts(&self) -> u32 {
        self.restarts.load(Ordering::Relaxed)
    }

    // ── The async twins ─────────────────────────────────────────

    /// (Re)create and start the workload's container from the
    /// carried configuration.
    pub async fn start_async(&self) -> Result<(), IntegrationError> {
        self.runtime.start(&self.cfg).await.map_err(runtime_err)
    }

    /// Stop the workload the dev-process way: the runtime's
    /// graceful-then-forcible signal stop (SIGTERM through the
    /// container's PID 1 — see [`crate::lifecycle`]).
    pub async fn stop_async(&self) -> Result<(), IntegrationError> {
        self.runtime
            .stop(self.stop_deadline)
            .await
            .map_err(runtime_err)
    }

    /// Attach the console pump: opens the runtime attachment and
    /// spawns the task that fills the retention buffer.
    ///
    /// Idempotent per run (the same posture the Minecraft handle
    /// takes). The pump clears the buffer first, so each run of
    /// the workload starts a fresh console history.
    pub async fn attach_console(&self) -> Result<(), IntegrationError> {
        if self.attached.load(Ordering::Acquire) {
            // The current run's pump is already live; a second
            // concurrent attach would duplicate every output line.
            return Ok(());
        }
        let attachment = self.runtime.attach_console().await.map_err(runtime_err)?;
        self.console.clear();
        self.attached.store(true, Ordering::Release);
        let console = Arc::clone(&self.console);
        let attached = Arc::clone(&self.attached);
        // The pump ends when the stream ends (the target exits;
        // the runtime's pump flushes its tail first).
        tokio::spawn(async move {
            let mut lines = attachment.lines;
            while let Some(line) = lines.recv().await {
                console.push_line(line);
            }
            attached.store(false, Ordering::Release);
        });
        Ok(())
    }

    /// Send one line of console input to the process (the dev
    /// loop's stdin surface).
    pub async fn send_input_async(&self, line: &str) -> Result<(), IntegrationError> {
        let mut payload = line.as_bytes().to_vec();
        payload.push(b'\n');
        self.runtime
            .send_input(&payload)
            .await
            .map_err(runtime_err)
    }

    /// One resource-usage sample.
    pub async fn metrics_async(&self) -> Result<SdkUsage, IntegrationError> {
        let usage = self.runtime.stats().await.map_err(runtime_err)?;
        Ok(to_sdk_usage(&usage))
    }

    // ── The watch-restart supervision ───────────────────────────

    /// Start the file watcher and the supervisor task that
    /// consumes its restart requests.
    ///
    /// The watcher covers the workload's whole data directory
    /// (the bind mount the process reads its sources through);
    /// the spec's patterns decide what matches. The supervisor
    /// task runs until the workload handle's clones are all
    /// dropped (the watcher's channel closes).
    ///
    /// A policy that does not watch (`Never`/`Always`) is a
    /// no-op: the §6 `Watching` state is the watching policy's
    /// own.
    pub fn supervise(&self) -> Result<(), IntegrationError> {
        if !self.spec.restart_policy.watches() {
            return Ok(());
        }
        let mut slot = self
            .watcher
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if slot.is_some() {
            // Already supervising.
            return Ok(());
        }
        let watcher = FileWatcher::start(self.cfg.data_dir.clone(), &self.spec)
            .map_err(|err| IntegrationError::Runtime(format!("the file watcher failed to start: {err}")))?;
        *slot = Some(watcher);
        drop(slot);

        // The supervisor task: poll the watcher's requests and
        // perform the restarts. The task carries a clone of the
        // handle (every field is Arc-shaped — the clone is cheap)
        // and holds only a weak reference to the watcher slot:
        // when the workload's last external clone drops, the
        // weak upgrade fails, the task exits, and the clone it
        // carried releases with it.
        let workload = self.clone();
        let watcher_weak = Arc::downgrade(&self.watcher);
        let io = self.io.clone();
        io.spawn(async move {
            loop {
                let request = {
                    let Some(watcher_slot) = watcher_weak.upgrade() else {
                        // The workload is gone; stop supervising.
                        break;
                    };
                    let slot = watcher_slot
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner());
                    match slot.as_ref() {
                        Some(watcher) => watcher.take_request(),
                        // The watcher was suspended; stop
                        // supervising.
                        None => break,
                    }
                };
                if let Some(request) = request {
                    // The watch-restart: graceful stop, the fresh
                    // console generation, relaunch, re-attach.
                    let _ = workload.stop_async().await;
                    workload.console.clear();
                    workload.attached.store(false, Ordering::Release);
                    match workload.start_async().await {
                        Ok(()) => {
                            workload.restarts.fetch_add(1, Ordering::Relaxed);
                            eprintln!(
                                "zim dev-process: watch-restart ({} file(s) changed: {:?})",
                                request.trigger_paths.len(),
                                request.trigger_paths
                            );
                            // The pump re-attaches for the new run
                            // (the console surface survives the
                            // restart).
                            if let Err(err) = workload.attach_console().await {
                                eprintln!(
                                    "zim dev-process: the restarted process's \
                                     console pump failed to attach: {err}"
                                );
                            }
                        }
                        Err(err) => {
                            eprintln!(
                                "zim dev-process: watch-restart relaunch failed: {err}"
                            );
                        }
                    }
                }
                tokio::time::sleep(SUPERVISOR_POLL_INTERVAL).await;
            }
        });
        Ok(())
    }

    /// Suspend the watcher (the §6 `Watching` state's exit).
    pub fn suspend_watch(&self) {
        let mut slot = self
            .watcher
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *slot = None;
    }

    /// The §6 `Watching` extended state.
    pub fn watch_state(&self) -> WatchState {
        let slot = self
            .watcher
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match slot.as_ref() {
            Some(watcher) => watcher.state(),
            None => WatchState::Suspended,
        }
    }

    /// The watcher's diagnostics (events seen, requests fired;
    /// zeros when not watching).
    pub fn watch_counters(&self) -> (u64, u64) {
        let slot = self
            .watcher
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match slot.as_ref() {
            Some(watcher) => (watcher.events_seen(), watcher.requests_fired()),
            None => (0, 0),
        }
    }

    /// Whether the workload's target is currently running.
    pub async fn is_running_async(&self) -> Result<bool, IntegrationError> {
        Ok(matches!(
            self.runtime.state().await.map_err(runtime_err)?,
            ObservedState::Running
        ))
    }
}

impl ConsoleCapable for DevProcessWorkload {
    fn console_tail(&self, lines: usize) -> Vec<String> {
        self.console.tail(lines)
    }

    fn send_input(&self, line: &str) -> Result<(), IntegrationError> {
        // Sync bridge: see the module docs.
        self.io.block_on(self.send_input_async(line))
    }
}

impl MetricsCapable for DevProcessWorkload {
    fn metrics(&self) -> Result<SdkUsage, IntegrationError> {
        // Sync bridge: see the module docs.
        self.io.block_on(self.metrics_async())
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
    fn the_usage_mapping_normalizes_the_runtime_sample() {
        let usage = RuntimeUsage {
            cpu_usage_total_ns: Some(500_000_000),
            system_cpu_total_ns: Some(1_000_000_000),
            online_cpus: Some(2),
            memory_usage_bytes: Some(1024),
            network_rx_bytes: Some(10),
            network_tx_bytes: Some(15),
        };
        let mapped = to_sdk_usage(&usage);
        assert!((mapped.cpu - 1.0).abs() < f32::EPSILON);
        assert_eq!(mapped.memory_bytes, 1024);
        assert_eq!(mapped.network_bytes, 25);
    }
}
