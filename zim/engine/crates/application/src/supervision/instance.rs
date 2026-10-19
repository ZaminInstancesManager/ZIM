//! The supervised instance: desired/observed state, the
//! single-worker power queue, the crash monitor, and the
//! runtime-facing [`InstanceConfig`] construction.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use chrono::Utc;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use zim_engine_domain::node::Allocation;
use zim_engine_domain::{Instance, InstanceId, InstanceStatus};

use crate::ports::Persistence;
use crate::supervision::crash::{CrashLoopPolicy, CrashState};
use crate::supervision::runtime::Runtime;
use crate::supervision::state::{
    DesiredState, InstanceConfig, ObservedState, PortBinding, ResourceUsage, RuntimeError,
};
use zim_engine_streaming::{EventData, ResourceUsageData, StreamHub};

/// Tunables of the supervision layer.
#[derive(Debug, Clone)]
pub struct SupervisionPolicy {
    /// Crash-loop detection parameters (invariant 14).
    pub crash: CrashLoopPolicy,
    /// How often the per-instance monitor polls the runtime while
    /// the instance is running. The event-driven form arrives with
    /// M7 streaming; 1 s matches Wings's stats cadence.
    pub monitor_poll_interval: Duration,
    /// The default graceful-stop deadline for stop/restart/suspend.
    pub stop_deadline: Duration,
    /// Whether the monitor publishes `instance.resource_usage`
    /// events on each poll (M7). Off only for tests that assert
    /// exact event sequences.
    pub stream_metrics: bool,
    /// How often the monitor *persists* a resource-usage sample
    /// (M30 telemetry ingestion). The stream keeps its live 1 s
    /// cadence; the durable series does not need per-second
    /// resolution, and the store should not carry it. 10 s is
    /// 8,640 rows/instance/day — comfortable under the 7-day
    /// retention window. `Duration::ZERO` disables persistence
    /// (tests that assert exact store contents).
    pub metrics_persist_interval: Duration,
}

impl Default for SupervisionPolicy {
    fn default() -> Self {
        Self {
            crash: CrashLoopPolicy::default(),
            monitor_poll_interval: Duration::from_secs(1),
            stop_deadline: Duration::from_secs(30),
            stream_metrics: true,
            metrics_persist_interval: Duration::from_secs(10),
        }
    }
}

/// A queued lifecycle operation (the power queue's item).
///
/// Wings's power queue carries the same four actions plus suspend;
/// the deadlines travel with the action so the worker that drains
/// the queue needs no extra coordination with the enqueue site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    /// Start the instance (recreating the container from the current
    /// configuration, Wings's `OnBeforeStart` behavior).
    Start,
    /// Gracefully stop, bounded by a deadline.
    Stop {
        /// How long to wait for a clean exit before killing.
        deadline: Duration,
    },
    /// Stop (bounded), then start.
    Restart {
        /// How long to wait for a clean exit before killing.
        deadline: Duration,
    },
    /// Immediately kill.
    Kill,
    /// Stop (bounded) and mark the instance suspended.
    Suspend {
        /// How long to wait for a clean exit before killing.
        deadline: Duration,
    },
}

/// Enqueue-time rejections for power requests.
///
/// These are the errors the API layer maps onto structured responses;
/// once an action is accepted it runs asynchronously and its outcome
/// is observable through the state endpoint (invariant 2).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PowerError {
    /// The instance is not supervised by this engine (unknown id, or
    /// on a node this engine does not manage).
    #[error("instance {0} is not supervised by this engine")]
    UnknownInstance(InstanceId),
    /// The instance is suspended; start/restart are rejected until an
    /// admin lifts the suspension.
    #[error("instance {0} is suspended; lifecycle-start is rejected")]
    Suspended(InstanceId),
    /// The runtime backend is unreachable (mapped to 503 by the API;
    /// observed state stays `Unknown`, never `Offline`).
    #[error("runtime unavailable: {0}")]
    RuntimeUnavailable(String),
    /// The power queue is closed (engine shutdown).
    #[error("power queue unavailable: {0}")]
    Backend(String),
}

/// A point-in-time view of a supervised instance's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedSnapshot {
    /// The instance.
    pub instance_id: InstanceId,
    /// The current desired lifecycle state.
    pub desired: DesiredState,
    /// The current observed lifecycle state.
    pub observed: ObservedState,
    /// The generation counter (bumped on every observed transition;
    /// stale-event protection anchor, invariant 13).
    pub generation: u64,
    /// Whether the crash-loop protection is engaged.
    pub in_crash_loop: bool,
    /// The persisted-status form: the observed state's status with
    /// the crash-loop overlay applied.
    pub status: InstanceStatus,
}

/// A supervised instance handle (cheap to clone; the state lives in
/// shared interior).
#[derive(Clone)]
pub struct SupervisedInstance {
    pub(crate) inner: Arc<InstanceInner>,
}

/// The shared supervision state for one instance.
pub(crate) struct InstanceInner {
    id: InstanceId,
    desired: RwLock<DesiredState>,
    observed: RwLock<ObservedState>,
    cfg: RwLock<InstanceConfig>,
    power_tx: mpsc::Sender<PowerAction>,
    runtime: Arc<dyn Runtime>,
    pub(crate) crash: CrashState,
    generation: AtomicU64,
    revision_tx: watch::Sender<u64>,
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,
    monitor: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The M7.1 console pump task (mirror of `monitor`'s replace
    /// slot: at most one pump runs per instance).
    console_pump: Mutex<Option<tokio::task::JoinHandle<()>>>,
    cancel: CancellationToken,
    pub(crate) policy: SupervisionPolicy,
    persistence: Arc<dyn Persistence>,
    /// The M7 streaming hub this instance publishes lifecycle and
    /// resource-usage events onto (RUST_ENGINE.md §4's
    /// `subscribers: StreamingSet` field, realized as the shared
    /// hub the streaming crate owns).
    stream: StreamHub,
}

impl SupervisedInstance {
    /// Build a supervised instance around an existing runtime handle
    /// and spawn its power worker.
    ///
    /// The initial state is *desired-stopped, observed-offline*
    /// regardless of the persisted row: a persisted `running` flag is
    /// not proof of liveness (invariant 1), so only reconciliation's
    /// runtime observation may move the instance out of the offline
    /// seed.
    pub(crate) fn spawn(
        cfg: InstanceConfig,
        runtime: Arc<dyn Runtime>,
        persistence: Arc<dyn Persistence>,
        policy: SupervisionPolicy,
        stream: StreamHub,
    ) -> Self {
        let (power_tx, power_rx) = mpsc::channel::<PowerAction>(16);
        let (revision_tx, _) = watch::channel(0u64);
        let cancel = CancellationToken::new();

        let inner = Arc::new(InstanceInner {
            id: cfg.instance_id,
            desired: RwLock::new(DesiredState::Stopped),
            observed: RwLock::new(ObservedState::Offline {
                exit_code: None,
                oom_killed: false,
            }),
            cfg: RwLock::new(cfg),
            power_tx,
            runtime,
            crash: CrashState::default(),
            generation: AtomicU64::new(0),
            revision_tx,
            worker: Mutex::new(None),
            monitor: Mutex::new(None),
            console_pump: Mutex::new(None),
            cancel: cancel.clone(),
            policy,
            persistence,
            stream,
        });

        let worker_inner = Arc::clone(&inner);
        let worker = tokio::spawn(async move {
            power_worker(worker_inner, power_rx).await;
        });
        *inner
            .worker
            .lock()
            .expect("supervision state poisoned by a panicking worker") = Some(worker);

        Self { inner }
    }

    /// The instance id.
    pub fn id(&self) -> InstanceId {
        self.inner.id
    }

    /// The current generation counter.
    pub fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::Acquire)
    }

    /// The current revision counter (bumped after every power action
    /// and every observed-state change; the supervision tests and
    /// API integration tests await it).
    pub fn revision(&self) -> u64 {
        *self.inner.revision_tx.borrow()
    }

    /// Wait until the revision reaches at least `n` (or the timeout
    /// expires; returns whether it was reached).
    pub async fn wait_revision(&self, n: u64, timeout: Duration) -> bool {
        let mut rx = self.inner.revision_tx.subscribe();
        if self.revision() >= n {
            return true;
        }
        match tokio::time::timeout(timeout, async {
            while rx.changed().await.is_ok() {
                if *rx.borrow_and_update() >= n {
                    break;
                }
            }
        })
        .await
        {
            Ok(()) => self.revision() >= n,
            Err(_) => false,
        }
    }

    /// The current state snapshot.
    pub fn snapshot(&self) -> ObservedSnapshot {
        let inner = &self.inner;
        let observed = inner
            .observed
            .read()
            .expect("supervision state poisoned by a panicking worker")
            .clone();
        let desired = *inner
            .desired
            .read()
            .expect("supervision state poisoned by a panicking worker");
        let in_crash_loop = inner.crash.in_crash_loop();
        let status = if in_crash_loop && matches!(observed, ObservedState::Offline { .. }) {
            InstanceStatus::CrashLoop
        } else {
            observed.status()
        };
        ObservedSnapshot {
            instance_id: inner.id,
            desired,
            observed,
            generation: inner.generation.load(Ordering::Acquire),
            in_crash_loop,
            status,
        }
    }

    /// The enqueue channel for power actions.
    pub fn power_tx(&self) -> mpsc::Sender<PowerAction> {
        self.inner.power_tx.clone()
    }

    /// Whether the instance's desired state is suspended.
    pub fn is_suspended(&self) -> bool {
        *self
            .inner
            .desired
            .read()
            .expect("supervision state poisoned by a panicking worker")
            == DesiredState::Suspended
    }

    /// Clear the crash state (the user-intervention path of
    /// invariant 14: an explicit start clears the crash loop).
    pub fn clear_crash_state(&self) {
        self.inner.crash.clear();
    }

    /// Write to the target's stdin (console input; M7.1). Writes go
    /// through the live console attachment — a target without one
    /// (not running) answers `NotRunning`.
    pub(crate) async fn send_input(
        &self,
        input: &[u8],
    ) -> Result<(), crate::supervision::state::RuntimeError> {
        self.inner.runtime.send_input(input).await
    }

    /// Refresh the runtime-facing configuration from a re-read
    /// persisted row (applied before the next start, so a patched
    /// image/limit/startup takes effect without an engine restart).
    pub fn refresh_config(&self, cfg: InstanceConfig) {
        *self
            .inner
            .cfg
            .write()
            .expect("supervision state poisoned by a panicking worker") = cfg;
    }

    /// Update the observed state directly (reconciliation path).
    pub(crate) fn set_observed_direct(&self, observed: ObservedState) {
        set_observed(&self.inner, observed);
    }

    /// Set the desired state directly (reconciliation path).
    pub(crate) fn set_desired_direct(&self, desired: DesiredState) {
        *self
            .inner
            .desired
            .write()
            .expect("supervision state poisoned by a panicking worker") = desired;
    }

    /// Query the runtime's current observation (reconciliation path;
    /// never trust the persisted row instead — invariant 1).
    pub(crate) async fn runtime_state(&self) -> Result<ObservedState, RuntimeError> {
        self.inner.runtime.state().await
    }

    /// Persist the current status from outside the power worker
    /// (reconciliation path).
    pub(crate) async fn persist_status_now(&self) {
        persist_status(&self.inner).await;
    }

    /// Record a crash at the current time (reconciliation path);
    /// returns whether the crash loop tripped.
    pub(crate) fn record_crash_now(&self) -> bool {
        self.inner
            .crash
            .record_crash(Utc::now(), &self.inner.policy.crash)
    }

    /// Arm the crash monitor from outside the power worker
    /// (reconciliation's adoption of an already-running target).
    pub(crate) fn arm_monitor_now(&self) {
        arm_monitor(&self.inner);
    }

    /// Arm the console pump from outside the power worker
    /// (reconciliation's adoption of an already-running target:
    /// a container started before this engine boot streams its
    /// post-adoption output onto the hub).
    pub(crate) fn arm_console_pump_now(&self) {
        arm_console_pump(&self.inner);
    }

    /// Stop the power worker, monitor, and console pump (engine
    /// shutdown).
    ///
    /// Running containers are left running on purpose (the engine is
    /// independently recoverable from its workloads; restart
    /// reconciliation re-adopts them on the next boot).
    pub async fn shutdown(&self) {
        self.inner.cancel.cancel();
        let worker = self
            .inner
            .worker
            .lock()
            .expect("supervision state poisoned by a panicking worker")
            .take();
        let monitor = self
            .inner
            .monitor
            .lock()
            .expect("supervision state poisoned by a panicking worker")
            .take();
        let console_pump = self
            .inner
            .console_pump
            .lock()
            .expect("supervision state poisoned by a panicking worker")
            .take();
        for handle in [worker, monitor, console_pump].into_iter().flatten() {
            // Bounded: a worker mid-action finishes its current
            // action at most within the stop deadline.
            let _ = tokio::time::timeout(self.inner.policy.stop_deadline * 2, handle).await;
        }
    }
}

/// The single-worker power queue loop (invariants 7 and 9).
///
/// Actions apply strictly one at a time per instance; concurrent
/// start/stop/restart/kill requests are sequenced here, never raced.
async fn power_worker(inner: Arc<InstanceInner>, mut rx: mpsc::Receiver<PowerAction>) {
    loop {
        let action = tokio::select! {
            maybe = rx.recv() => match maybe {
                Some(action) => action,
                None => break,
            },
            _ = inner.cancel.cancelled() => break,
        };
        apply_power_action(&inner, action).await;
        bump_revision(&inner);
    }
}

/// Apply one power action end-to-end (runs on the worker task only).
async fn apply_power_action(inner: &Arc<InstanceInner>, action: PowerAction) {
    if std::env::var("ZIM_DBG").is_ok() {
        eprintln!(
            "DBG action {:?} (observed {:?})",
            action,
            current_observed(inner)
        );
    }
    match action {
        PowerAction::Start => apply_start(inner).await,
        PowerAction::Stop { deadline } => apply_stop(inner, deadline, DesiredState::Stopped).await,
        PowerAction::Restart { deadline } => {
            apply_stop(inner, deadline, DesiredState::Stopped).await;
            apply_start(inner).await;
        }
        PowerAction::Kill => apply_kill(inner).await,
        PowerAction::Suspend { deadline } => {
            apply_stop(inner, deadline, DesiredState::Suspended).await
        }
    }
}

/// `Start`: mark Starting, (re)create + start the container, observe,
/// and arm the crash monitor.
async fn apply_start(inner: &Arc<InstanceInner>) {
    // Suspend wins over start (the enqueue layer rejects user starts;
    // this guard also covers auto-restarts racing an admin suspend).
    if read_desired(inner) == DesiredState::Suspended {
        return;
    }

    set_desired(inner, DesiredState::Started);
    set_observed(inner, ObservedState::Starting);
    persist_status(inner).await;

    let cfg = inner
        .cfg
        .read()
        .expect("supervision state poisoned by a panicking worker")
        .clone();
    match inner.runtime.start(&cfg).await {
        Ok(()) => {
            // A successful start means the runtime accepted the
            // container as running; take the runtime's own observation
            // for the details (started_at, pid).
            let observed = match inner.runtime.state().await {
                Ok(s) if s.is_running() => s,
                _ => ObservedState::Running {
                    started_at: Utc::now(),
                    pid: None,
                },
            };
            set_observed(inner, observed);
            persist_status(inner).await;
            arm_monitor(inner);
            arm_console_pump(inner);
        }
        Err(e) => {
            // A failed start is a failed operation, not a process
            // crash: no crash history is recorded. An unreachable
            // backend surfaces as Unknown (never Offline).
            let observed = if e.is_unavailable() {
                ObservedState::Unknown
            } else {
                ObservedState::Offline {
                    exit_code: None,
                    oom_killed: false,
                }
            };
            set_observed(inner, observed);
            persist_status(inner).await;
            eprintln!(
                "zim_engine: supervision: instance {} start failed: {e}",
                inner.id
            );
        }
    }
}

/// `Stop`/`Suspend`: graceful stop bounded by the deadline, then
/// observe the exit state.
async fn apply_stop(inner: &Arc<InstanceInner>, deadline: Duration, final_desired: DesiredState) {
    let was = current_observed(inner);
    set_desired(inner, final_desired);

    let stopping = matches!(was, ObservedState::Running { .. } | ObservedState::Starting);
    if std::env::var("ZIM_DBG").is_ok() {
        eprintln!("DBG   stop: was={:?} stopping={}", was, stopping);
    }
    if stopping {
        set_observed(inner, ObservedState::Stopping { since: Utc::now() });
        persist_status(inner).await;
    }

    if stopping {
        match inner.runtime.stop(deadline).await {
            Ok(()) | Err(_) => {
                // Both paths fall through to re-observation; the
                // Docker adapter's stop() already escalates to a
                // kill after the deadline, and not-found is the
                // already-stopped outcome.
                let (exit_code, oom_killed) =
                    inner.runtime.exit_state().await.unwrap_or((None, false));
                set_observed(
                    inner,
                    ObservedState::Offline {
                        exit_code,
                        oom_killed,
                    },
                );
            }
        }
    } else {
        // Already not running: idempotent stop; still refresh the
        // exit details if a stale container record exists.
        let (exit_code, oom_killed) = inner.runtime.exit_state().await.unwrap_or((None, false));
        set_observed(
            inner,
            ObservedState::Offline {
                exit_code,
                oom_killed,
            },
        );
    }

    persist_status(inner).await;
}

/// `Kill`: immediate forcible termination.
async fn apply_kill(inner: &Arc<InstanceInner>) {
    // Mark stopping *before* the kill so a monitor sample in flight
    // cannot classify the kill as a crash (the generation bump makes
    // the stale sample discard; desired-flip makes even a
    // post-transition sample read as intentional).
    set_desired(inner, DesiredState::Killed);
    set_observed(inner, ObservedState::Stopping { since: Utc::now() });

    let _ = inner.runtime.kill().await;
    let (exit_code, oom_killed) = inner.runtime.exit_state().await.unwrap_or((None, false));
    set_desired(inner, DesiredState::Stopped);
    set_observed(
        inner,
        ObservedState::Offline {
            exit_code,
            oom_killed,
        },
    );
    persist_status(inner).await;
}

/// Arm (or re-arm) the per-instance crash monitor.
///
/// At most one monitor runs per instance: arming replaces any
/// previous handle after aborting it (a restart's new monitor must
/// not double-count with the old one's final sample).
fn arm_monitor(inner: &Arc<InstanceInner>) {
    let mut slot = inner
        .monitor
        .lock()
        .expect("supervision state poisoned by a panicking worker");
    if let Some(previous) = slot.take() {
        previous.abort();
    }
    let monitor_inner = Arc::clone(inner);
    *slot = Some(tokio::spawn(async move {
        monitor_loop(monitor_inner).await;
    }));
}

/// Arm (or re-arm) the per-instance console pump (M7.1).
///
/// Mirrors [`arm_monitor`]'s replace semantics: at most one pump
/// runs per instance, and arming aborts any previous pump after
/// taking the slot — a restart's new pump never double-pumps with
/// the old one's tail (any stale lines from the destroyed container
/// are dropped, which is exactly what "no double-pumps" wants).
fn arm_console_pump(inner: &Arc<InstanceInner>) {
    let mut slot = inner
        .console_pump
        .lock()
        .expect("supervision state poisoned by a panicking worker");
    if let Some(previous) = slot.take() {
        previous.abort();
    }
    let pump_inner = Arc::clone(inner);
    *slot = Some(tokio::spawn(async move {
        console_pump_loop(pump_inner).await;
    }));
}

/// The M7.1 console pump: attach to the runtime's console, publish
/// each decoded line onto the streaming hub as an
/// `instance.console_output` event, and end when the attachment's
/// stream ends (the target exited; the runtime's pump flushes its
/// partial-line tail first) or the instance is shut down.
///
/// The pump obeys the instance's supervision lifetime — it arms when
/// the target enters `Running`, ends on exit, and is replaced on
/// restart — and it never blocks the runtime: hub publishes are
/// synchronous and bounded (the hub drops events under subscriber
/// backpressure, §11.2), and the runtime's own pump task only awaits
/// its attach stream and the bounded channel.
async fn console_pump_loop(inner: Arc<InstanceInner>) {
    let mut lines = match inner.runtime.attach_console().await {
        Ok(attachment) => attachment.lines,
        Err(e) => {
            // Not-running is the quiet case (the target died between
            // observation and attach, or the runtime has no console
            // capability); unavailable runtimes surface through the
            // monitor's Unknown path. Anything else is worth a line
            // in the log but never fails supervision.
            if !e.is_unavailable() && !matches!(e, RuntimeError::NotRunning(_)) {
                eprintln!(
                    "zim_engine: supervision: instance {} console attach failed: {e}",
                    inner.id
                );
            }
            return;
        }
    };

    loop {
        let line = tokio::select! {
            _ = inner.cancel.cancelled() => break,
            line = lines.recv() => match line {
                Some(line) => line,
                None => break, // the attachment ended: flushed and done
            },
        };
        inner
            .stream
            .publish(inner.id, EventData::ConsoleOutput { line });
    }
}

/// The M6 crash monitor: poll the runtime while the instance runs,
/// detect exits, and drive crash protection (invariant 14) with
/// stale-sample protection (invariant 13).
///
/// M7 replaces polling with runtime events; the invariant logic
/// (crash counting, crash-loop parking, auto-restart) stays.
async fn monitor_loop(inner: Arc<InstanceInner>) {
    // M30: when the monitor last persisted a telemetry sample
    // (None = the next running sample persists immediately, so a
    // freshly-started instance's series begins at its start).
    let mut last_persist: Option<std::time::Instant> = None;
    loop {
        tokio::select! {
            _ = inner.cancel.cancelled() => break,
            _ = tokio::time::sleep(inner.policy.monitor_poll_interval) => {}
        }

        // Invariant 13: a sample is only valid if no transition
        // happened while it was in flight.
        let gen_before = inner.generation.load(Ordering::Acquire);
        let sample = inner.runtime.state().await;
        if inner.generation.load(Ordering::Acquire) != gen_before {
            continue;
        }

        if std::env::var("ZIM_DBG").is_ok() {
            eprintln!(
                "DBG   monitor sample {:?} (observed {:?})",
                sample,
                current_observed(&inner)
            );
        }
        match sample {
            Ok(state) => match state {
                s if s.is_running() => {
                    // Still running: nothing to do state-wise. M7
                    // streams a resource-usage sample per tick
                    // (Wings's stats cadence); a failed sample is
                    // skipped, not fatal — telemetry must never
                    // trip the monitor. M30 doubles the consumer:
                    // the same observation feeds the durable series
                    // at the telemetry cadence (the store's rows,
                    // the history endpoint's source).
                    let usage = if inner.policy.stream_metrics
                        || inner.policy.metrics_persist_interval > Duration::ZERO
                    {
                        inner.runtime.stats().await.ok()
                    } else {
                        None
                    };
                    if let Some(usage) = usage {
                        if inner.policy.stream_metrics {
                            inner
                                .stream
                                .publish(inner.id, EventData::ResourceUsage(usage_to_wire(&usage)));
                        }
                        let due = inner.policy.metrics_persist_interval > Duration::ZERO
                            && match last_persist {
                                None => true,
                                Some(at) => at.elapsed() >= inner.policy.metrics_persist_interval,
                            };
                        if due {
                            last_persist = Some(std::time::Instant::now());
                            persist_telemetry_sample(&inner, &usage);
                        }
                    }
                }
                ObservedState::Offline {
                    exit_code,
                    oom_killed,
                } => {
                    // A death is only a Running-to-Offline transition.
                    // While the power worker owns a transitional state
                    // (a start in flight — including a restart's start
                    // phase, where a *previous* start's monitor is
                    // still armed — or a stop in flight), an Offline
                    // sample is expected progress and the worker's own
                    // outcome handling owns it. Acting on it here
                    // would misread the pre-start window as a crash.
                    if !current_observed(&inner).is_running() {
                        break;
                    }
                    let desired = read_desired(&inner);
                    if desired.wants_running() {
                        handle_crash(&inner, exit_code, oom_killed).await;
                    } else {
                        // An intentional (or already-reflected) stop.
                        set_observed(
                            &inner,
                            ObservedState::Offline {
                                exit_code,
                                oom_killed,
                            },
                        );
                    }
                    break;
                }
                ObservedState::Unknown => {
                    // Same guard: an unreachable backend matters when
                    // the instance was believed running; transitional
                    // states have the worker's own error handling.
                    if current_observed(&inner).is_running() {
                        set_observed(&inner, ObservedState::Unknown);
                    }
                    break;
                }
                // Starting/Stopping samples never come from a
                // container runtime (they are engine-side states);
                // keep polling defensively.
                _ => {}
            },
            Err(e) if e.is_unavailable() => {
                set_observed(&inner, ObservedState::Unknown);
                break;
            }
            Err(_) => {
                // Transient backend error: keep polling (a blip is
                // not an exit, and not Unknown either).
            }
        }
    }
}

/// Crash handling for an observed exit while desired-running
/// (Wings's `handleServerCrash` analogue).
///
/// M6 follows Wings's default `DetectCleanExitAsCrash = true`: *any*
/// exit while desired-started counts as a crash, including a clean
/// one — that is what prevents boot-looping one-shot containers.
#[allow(clippy::too_many_lines)]
async fn handle_crash(inner: &Arc<InstanceInner>, exit_code: Option<i32>, oom_killed: bool) {
    let enabled = inner
        .cfg
        .read()
        .expect("supervision state poisoned by a panicking worker")
        .crash_detection_enabled;
    set_observed(
        inner,
        ObservedState::Offline {
            exit_code,
            oom_killed,
        },
    );

    if !enabled {
        eprintln!(
            "zim_engine: supervision: instance {} exited (code {exit_code:?}, oom {oom_killed}); \
             crash detection disabled — no auto-restart",
            inner.id
        );
        persist_status(inner).await;
        return;
    }

    let tripped = inner.crash.record_crash(Utc::now(), &inner.policy.crash);
    if tripped {
        eprintln!(
            "zim_engine: supervision: instance {} crash-loop engaged ({} crashes within {:?}); \
             auto-restart suspended until a user start",
            inner.id, inner.policy.crash.threshold, inner.policy.crash.window
        );
        persist_status(inner).await; // CrashLoop overlay applies in persist path
        return;
    }

    persist_status(inner).await;
    if inner.policy.crash.auto_restart {
        // Best-effort self-enqueue; a full queue gives up (logged)
        // rather than blocking the monitor.
        if inner.power_tx.send(PowerAction::Start).await.is_err() {
            eprintln!(
                "zim_engine: supervision: instance {} crash auto-restart dropped \
                 (power queue closed)",
                inner.id
            );
        }
    }
}

// ── Small state helpers (all lock-then-release; never across await) ──

/// Persist one telemetry sample (M30): the runtime's observation as
/// a domain [`MetricsSample`] row, appended through the persistence
/// port on a detached blocking task — a stalled store write must
/// never stall the monitor (the M3 sync-port discipline: the port
/// is `Send + Sync` for exactly this hop). A failed append is
/// logged and dropped, not retried: the next cadence tick brings
/// the next sample, and a hole in the series is honest (the
/// derivation path treats consecutive-row deltas, so a missing row
/// widens one interval rather than corrupting it).
fn persist_telemetry_sample(inner: &InstanceInner, usage: &ResourceUsage) {
    let sample = zim_engine_domain::MetricsSample {
        instance_id: inner.id,
        ts: Utc::now(),
        cpu_usage_total_ns: usage.cpu_usage_total_ns,
        system_cpu_total_ns: usage.system_cpu_total_ns,
        online_cpus: usage.online_cpus,
        memory_usage_bytes: usage.memory_usage_bytes,
        memory_limit_bytes: usage.memory_limit_bytes,
        pids_current: usage.pids_current,
        network_rx_bytes: usage.network_rx_bytes,
        network_tx_bytes: usage.network_tx_bytes,
    };
    let persistence = Arc::clone(&inner.persistence);
    tokio::task::spawn_blocking(move || {
        if let Err(e) = persistence.append_metrics_sample(&sample) {
            eprintln!("zim_engine: supervision: telemetry sample append failed: {e}");
        }
    });
}

/// Map the runtime's resource-usage observation onto the streaming
/// crate's wire form (the application layer cannot leak its own
/// types downward through the crate graph).
fn usage_to_wire(usage: &ResourceUsage) -> ResourceUsageData {
    ResourceUsageData {
        cpu_usage_total_ns: usage.cpu_usage_total_ns,
        system_cpu_total_ns: usage.system_cpu_total_ns,
        online_cpus: usage.online_cpus,
        memory_usage_bytes: usage.memory_usage_bytes,
        memory_limit_bytes: usage.memory_limit_bytes,
        pids_current: usage.pids_current,
    }
}

fn read_desired(inner: &InstanceInner) -> DesiredState {
    *inner
        .desired
        .read()
        .expect("supervision state poisoned by a panicking worker")
}

fn set_desired(inner: &InstanceInner, desired: DesiredState) {
    *inner
        .desired
        .write()
        .expect("supervision state poisoned by a panicking worker") = desired;
}

fn current_observed(inner: &InstanceInner) -> ObservedState {
    inner
        .observed
        .read()
        .expect("supervision state poisoned by a panicking worker")
        .clone()
}

/// Set the observed state and bump the generation (the transition
/// counter — invariant 13's anchor), publishing an
/// `instance.state_change` event when the status vocabulary actually
/// changed (M7; ZIM_API_SPEC.md §11.1).
///
/// No-op writes (the same observed state re-asserted by reconcile)
/// still bump the supervision generation — the transition counter
/// guards runtime-sample races, not the event stream — but emit no
/// event, because a stream subscriber must see *transitions*, not
/// re-assertions.
fn set_observed(inner: &InstanceInner, observed: ObservedState) {
    let from = current_observed(inner);
    *inner
        .observed
        .write()
        .expect("supervision state poisoned by a panicking worker") = observed.clone();
    inner.generation.fetch_add(1, Ordering::AcqRel);
    bump_revision(inner);
    if from.status() != observed.status() {
        inner.stream.publish(
            inner.id,
            EventData::StateChange {
                from: from.status(),
                to: observed.status(),
            },
        );
    }
}

fn bump_revision(inner: &InstanceInner) {
    inner.revision_tx.send_modify(|v| *v += 1);
}

/// Persist the current status (crash-loop overlay applied).
///
/// Best-effort by design: a persistence failure is logged and
/// swallowed — the in-memory state remains authoritative for
/// supervision, and the next transition (or the next boot's
/// reconciliation) re-attempts the write. This mirrors the M5
/// audit-append policy.
async fn persist_status(inner: &InstanceInner) {
    let in_crash_loop = inner.crash.in_crash_loop();
    let observed = current_observed(inner);
    let status = if in_crash_loop && matches!(observed, ObservedState::Offline { .. }) {
        InstanceStatus::CrashLoop
    } else if read_desired(inner) == DesiredState::Suspended
        && matches!(observed, ObservedState::Offline { .. })
    {
        InstanceStatus::Suspended
    } else {
        observed.status()
    };

    let id = inner.id;
    let persistence = Arc::clone(&inner.persistence);
    let result =
        tokio::task::spawn_blocking(move || persistence.update_instance_status(&id, status)).await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => eprintln!(
            "zim_engine: supervision: instance {} status persist failed: {e}",
            inner.id
        ),
        Err(e) => eprintln!(
            "zim_engine: supervision: instance {} status persist task failed: {e}",
            inner.id
        ),
    }
}

// ── Configuration construction ────────────────────────────────────

/// Build the runtime-facing [`InstanceConfig`] from a persisted row
/// and its allocations.
///
/// # M6 simplifications (documented per the M5 delivery-note style)
///
/// - Startup rendering substitutes only the universally-known
///   variables (`SERVER_MEMORY`, `SERVER_PORT`, `SERVER_IP`); the
///   full egg-variable surface (`{{VAR}}` from egg variables) lands
///   with M9's egg importer.
/// - The entrypoint is `None` (images carry their own; eggs may
///   override it from M9).
/// - `SERVER_IP` keeps the allocation's literal address; Wings's
///   127.0.0.1-to-network-gateway rewrite is a Docker-factory
///   concern and is applied there.
pub fn build_instance_config(
    instance: &Instance,
    primary: &Allocation,
    extra_allocations: &[Allocation],
    data_root: &Path,
) -> Result<InstanceConfig, RuntimeError> {
    let command = render_startup(instance, primary)?;
    let mib = instance.limits.memory.0 / (1024 * 1024);

    let mut env = vec![
        ("SERVER_MEMORY".to_owned(), mib.to_string()),
        ("SERVER_PORT".to_owned(), primary.port.to_string()),
        ("SERVER_IP".to_owned(), primary.ip.to_string()),
        // Wings-parity basics the images expect.
        ("HOME".to_owned(), "/home/container".to_owned()),
        ("USER".to_owned(), "container".to_owned()),
    ];
    env.shrink_to_fit();

    let mut port_bindings = Vec::with_capacity(1 + extra_allocations.len());
    port_bindings.push(PortBinding {
        host_ip: primary.ip,
        host_port: primary.port,
        container_port: primary.port,
        protocol: Default::default(),
    });
    for allocation in extra_allocations {
        port_bindings.push(PortBinding {
            host_ip: allocation.ip,
            host_port: allocation.port,
            container_port: allocation.port,
            protocol: Default::default(),
        });
    }

    let data_dir = instance_data_dir(data_root, &instance.uuid_short);

    Ok(InstanceConfig {
        instance_id: instance.id,
        container_name: instance.uuid.clone(),
        hostname: instance.uuid.clone(),
        image: instance.image.to_string(),
        entrypoint: None,
        command,
        env,
        limits: instance.limits.clone(),
        port_bindings,
        data_dir,
        container_data_dir: PathBuf::from("/home/container"),
        labels: vec![
            ("Service".to_owned(), "ZIM".to_owned()),
            ("ContainerType".to_owned(), "server_process".to_owned()),
            ("ZIM.InstanceId".to_owned(), instance.id.to_string()),
        ],
        crash_detection_enabled: instance.crash_detection_enabled,
        // M18's spec-driven per-instance overrides: the persisted
        // row has no user/rootfs posture of its own yet (the
        // workload spec carries it); the database integrations fill
        // these when they build their own configs, and the
        // spec-less game-server path keeps the runtime defaults.
        user: None,
        readonly_rootfs: None,
        cap_drop: None,
    })
}

/// The per-instance data directory under the engine's data root
/// (short-UUID named, Pterodactyl's convention).
pub fn instance_data_dir(data_root: &Path, uuid_short: &str) -> PathBuf {
    data_root.join("instances").join(uuid_short)
}

/// Render the startup template to an argv (the M6 variable subset).
fn render_startup(instance: &Instance, primary: &Allocation) -> Result<Vec<String>, RuntimeError> {
    let mib = instance.limits.memory.0 / (1024 * 1024);
    let rendered = instance
        .startup
        .as_str()
        .replace("{{SERVER_MEMORY}}", &mib.to_string())
        .replace("{{SERVER_PORT}}", &primary.port.to_string())
        .replace("{{SERVER_IP}}", &primary.ip.to_string());
    let words = shlex::split(&rendered).ok_or_else(|| {
        RuntimeError::InvalidConfig(format!(
            "startup command has unbalanced quotes: {rendered:?}"
        ))
    })?;
    if words.is_empty() {
        return Err(RuntimeError::InvalidConfig(
            "startup command is empty".to_owned(),
        ));
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_instance() -> Instance {
        let (uuid, uuid_short) = Instance::legacy_uuid_pair();
        Instance {
            id: InstanceId::new(),
            external_id: None,
            uuid,
            uuid_short,
            node_id: zim_engine_domain::NodeId::new(),
            name: "Test".to_owned(),
            description: String::new(),
            status: InstanceStatus::Offline,
            skip_egg_scripts: false,
            owner_id: zim_engine_domain::UserId::new(),
            limits: Default::default(),
            allocation_id: zim_engine_domain::AllocationId::new(1),
            egg_id: zim_engine_domain::EggId::new(),
            startup: zim_engine_domain::CommandTemplate::new(
                "java -Xmx{{SERVER_MEMORY}}M -jar {{SERVER_JAR}}",
            ),
            image: "ghcr.io/parkervcp/yolks:java_21".parse().unwrap(),
            allocation_limit: None,
            database_limit: None,
            backup_limit: 0,
            crash_detection_enabled: true,
            installed_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn sample_allocation() -> Allocation {
        Allocation {
            id: zim_engine_domain::AllocationId::new(1),
            node_id: zim_engine_domain::NodeId::new(),
            ip: "127.0.0.1".parse().unwrap(),
            port: 25565,
            assigned_to: None,
            note: None,
        }
    }

    #[test]
    fn startup_template_renders_known_variables() {
        let instance = sample_instance();
        let allocation = sample_allocation();
        let argv = render_startup(&instance, &allocation).unwrap();
        // {{SERVER_MEMORY}} = 0 MiB (unlimited default) and the
        // unknown {{SERVER_JAR}} is left verbatim for M9.
        assert_eq!(argv[0], "java");
        assert_eq!(argv[1], "-Xmx0M");
        assert_eq!(argv[2], "-jar");
        assert_eq!(argv[3], "{{SERVER_JAR}}");
    }

    #[test]
    fn startup_template_rejects_empty_commands() {
        let mut instance = sample_instance();
        instance.startup = zim_engine_domain::CommandTemplate::new("   ");
        let err = render_startup(&instance, &sample_allocation()).unwrap_err();
        assert!(matches!(err, RuntimeError::InvalidConfig(_)));
    }

    #[test]
    fn startup_template_rejects_unbalanced_quotes() {
        let mut instance = sample_instance();
        instance.startup = zim_engine_domain::CommandTemplate::new("sh -c 'oops");
        let err = render_startup(&instance, &sample_allocation()).unwrap_err();
        assert!(matches!(err, RuntimeError::InvalidConfig(_)));
    }

    #[test]
    fn instance_config_carries_wings_parity() {
        let instance = sample_instance();
        let config =
            build_instance_config(&instance, &sample_allocation(), &[], Path::new("/data"))
                .unwrap();
        assert_eq!(config.container_name, instance.uuid);
        assert_eq!(config.hostname, instance.uuid);
        assert_eq!(config.image, "ghcr.io/parkervcp/yolks:java_21".to_owned());
        assert_eq!(
            config.env.iter().find(|(k, _)| k == "SERVER_PORT"),
            Some(&("SERVER_PORT".to_owned(), "25565".to_owned()))
        );
        assert_eq!(
            config.env.iter().find(|(k, _)| k == "SERVER_IP"),
            Some(&("SERVER_IP".to_owned(), "127.0.0.1".to_owned()))
        );
        assert_eq!(config.port_bindings.len(), 1);
        assert_eq!(config.port_bindings[0].host_port, 25565);
        assert_eq!(
            config.labels.iter().find(|(k, _)| k == "ContainerType"),
            Some(&("ContainerType".to_owned(), "server_process".to_owned()))
        );
        assert_eq!(
            config.data_dir,
            PathBuf::from("/data/instances").join(&instance.uuid_short)
        );
        assert_eq!(config.container_data_dir, PathBuf::from("/home/container"));
        assert!(config.crash_detection_enabled);
    }

    #[test]
    fn extra_allocations_become_port_bindings() {
        let instance = sample_instance();
        let mut extra = sample_allocation();
        extra.port = 25566;
        let config = build_instance_config(
            &instance,
            &sample_allocation(),
            &[extra.clone()],
            Path::new("/d"),
        )
        .unwrap();
        assert_eq!(config.port_bindings.len(), 2);
        assert_eq!(config.port_bindings[1].host_port, 25566);
        assert_eq!(config.port_bindings[1].container_port, 25566);
    }
}
