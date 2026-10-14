//! The executable half of the transfer system: the
//! `JobKind::Transfer` handler walking the two-phase-commit state
//! machine.
//!
//! ```text
//! Pending → SourceStopping → SourceArchiving → ArchiveTransferring
//!         → DestExtracting → DestStarting → SourceCleanup → Succeeded
//! ```
//!
//! Every phase boundary is persisted on the [`Transfer`] row
//! before the phase's side effects begin, so a crash (or the
//! boot-time orphan reconciliation that follows one) always leaves
//! a row that says exactly which phase was in flight. The phases:
//!
//! - **SourceStopping** — stop the source workload through the
//!   supervision power queue (invariant 9's serialization: the
//!   transfer's stop lines up behind whatever the queue is already
//!   doing). The pre-transfer power state is remembered and
//!   restored on the way out of any pre-commit failure.
//! - **SourceArchiving** — archive the instance's data directory
//!   with the shared backup archive engine (tar+gzip, streaming
//!   sha256, atomic `.part` → rename finalize).
//! - **ArchiveTransferring** — ship the archive from the source
//!   half of the transfer tree to the destination half and verify
//!   the digest on arrival. In this milestone the "ship" is a
//!   same-filesystem rename (both halves live under the engine's
//!   data root); when the node-runtime contract lands, the bytes
//!   cross the wire here instead. The verification is the point:
//!   the destination never trusts unverified bytes.
//! - **DestExtracting** — digest-verified, escape-safe extraction
//!   into a staging tree, then the *commit*: the live directory's
//!   contents are swapped in place (the directory itself is never
//!   renamed — a live bind mount pins it).
//! - **DestStarting** — rebind the instance onto the destination
//!   node and its new allocation (the persisted row is written
//!   before the start request, so the recreated container binds
//!   the destination allocation), then start the workload if (and
//!   only if) it was running before the transfer.
//! - **SourceCleanup** — delete the transfer tree; nothing else
//!   remains of the source side (the live directory *is* the
//!   destination's data now).
//!
//! # Failure semantics (roadmap M8, invariant 11)
//!
//! A failure before the commit leaves the live directory untouched
//! and the transfer tree half-populated: the job answers `Failed`
//! with `cleanup_pending = true` and the background cleaner
//! removes the tree. A failure after the commit leaves
//! content-equivalent data in place (the swap only ever installs
//! the digest-verified copy of what was already there) — the
//! instance stays runnable and rebound; the power-restore path is
//! not retried there (the destination start already tried once;
//! the operator's start is the honest next step). Either way the
//! source instance's running state survives the failure.
//!
//! # Cancellation
//!
//! Cancellation is honored at the safe points *before* the commit:
//! the long phases poll the token (the archive engine's
//! `is_cancelled` hooks), and the phase boundaries check it. After
//! the commit the move is already content-complete, so the handler
//! finishes the remaining bookkeeping (rebind is already persisted,
//! the start proceeds, the tree is cleaned) rather than abandoning
//! a half-told story.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use zim_engine_application::ports::Persistence;
use zim_engine_application::supervision::{
    instance_data_dir, ObservedSnapshot, ObservedState, PowerAction, Supervisor,
};
use zim_engine_backups::{
    create_archive, restore_archive, swap_contents_in_place, verify_archive, ArchiveError,
};
use zim_engine_domain::job::Job;
use zim_engine_domain::status::TransferState;
use zim_engine_domain::{Allocation, AllocationId, Instance, InstanceId, Transfer, TransferId};

use zim_engine_jobs::{JobContext, JobHandler, JobOutcome};

use crate::store::TransferStore;

/// Phase timing knobs. The defaults bound the whole state machine
/// well under the executor's shutdown grace; tests tighten them.
#[derive(Debug, Clone)]
pub struct TransferPhasePolicy {
    /// How long a graceful stop may take before the power worker
    /// kills the workload (handed to `PowerAction::Stop`).
    pub stop_deadline: Duration,
    /// How long the handler waits for the observed state to reach
    /// `Offline` after requesting (or observing) a stop.
    pub stop_wait: Duration,
    /// How long the handler waits for `Running` after the
    /// destination-side start request.
    pub start_wait: Duration,
    /// How long the failure-path power restore waits for `Running`.
    pub restart_wait: Duration,
    /// The observed-state poll cadence.
    pub poll_interval: Duration,
}

impl Default for TransferPhasePolicy {
    fn default() -> Self {
        Self {
            stop_deadline: Duration::from_secs(30),
            stop_wait: Duration::from_secs(90),
            start_wait: Duration::from_secs(90),
            restart_wait: Duration::from_secs(60),
            poll_interval: Duration::from_millis(200),
        }
    }
}

/// The transfer job handler (see the module docs).
pub struct TransferJobHandler {
    persistence: Arc<dyn Persistence>,
    supervisor: Arc<Supervisor>,
    store: TransferStore,
    data_root: PathBuf,
    policy: TransferPhasePolicy,
}

/// The rows a transfer job operates on, resolved once at claim
/// time (the state machine re-reads nothing it can avoid; the
/// allocation re-check at commit time is the deliberate
/// exception).
struct Resolved {
    transfer: Transfer,
    instance: Instance,
    allocation: Allocation,
}

/// How a bounded wait ended.
enum Waited {
    /// The target state was observed.
    Reached,
    /// The deadline passed first.
    TimedOut,
    /// Cancellation was observed while waiting.
    Cancelled,
    /// The supervisor could not answer.
    Failed(String),
}

/// The unwind bookkeeping a failure path needs: whether the
/// workload was running when the transfer began, whether this
/// handler requested (and confirmed) a stop, and whether the
/// in-place commit has since installed the destination data.
#[derive(Debug, Clone, Copy, Default)]
struct Unwind {
    /// The observed pre-transfer power state.
    was_running: bool,
    /// Whether the handler's stop request was accepted (the queue
    /// took it) and the workload reached `Offline`.
    stopped_for_transfer: bool,
    /// Whether the digest-verified tree is already installed in
    /// the live directory (post-commit failures do not retry the
    /// power restore — see the module docs).
    committed: bool,
}

impl Unwind {
    /// Whether the failure path should bring the workload back up.
    fn wants_restore(&self) -> bool {
        self.was_running && self.stopped_for_transfer && !self.committed
    }
}

impl TransferJobHandler {
    /// Build the handler; [`crate::TransferService::register`]
    /// installs it on the executor.
    pub fn new(
        persistence: Arc<dyn Persistence>,
        supervisor: Arc<Supervisor>,
        store: TransferStore,
        data_root: PathBuf,
    ) -> Self {
        Self {
            persistence,
            supervisor,
            store,
            data_root,
            policy: TransferPhasePolicy::default(),
        }
    }

    /// Build the handler with explicit phase timings (tests).
    pub fn with_policy(
        persistence: Arc<dyn Persistence>,
        supervisor: Arc<Supervisor>,
        store: TransferStore,
        data_root: PathBuf,
        policy: TransferPhasePolicy,
    ) -> Self {
        Self {
            persistence,
            supervisor,
            store,
            data_root,
            policy,
        }
    }

    // ── Row resolution ─────────────────────────────────────────

    /// The transfer row this job drives: from the job's JSON
    /// payload (`{"transfer_id": "..."}`), falling back to the
    /// instance's rows linked by `job_id` (the companion-entity
    /// pattern works in both directions).
    fn transfer_for_job_sync(persistence: &dyn Persistence, job: &Job) -> Option<Transfer> {
        if let Some(payload) = job.payload.as_deref() {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) {
                if let Some(text) = value.get("transfer_id").and_then(|t| t.as_str()) {
                    if let Ok(id) = text.parse::<TransferId>() {
                        if let Ok(found) = persistence.get_transfer(&id) {
                            return found;
                        }
                    }
                }
            }
        }
        persistence
            .list_transfers_by_instance(&job.instance_id)
            .ok()?
            .into_iter()
            .find(|t| t.job_id == job.id)
    }

    /// The landing allocation the payload names.
    fn allocation_for_job_sync(persistence: &dyn Persistence, job: &Job) -> Option<Allocation> {
        let payload = job.payload.as_deref()?;
        let value: serde_json::Value = serde_json::from_str(payload).ok()?;
        let text = value.get("allocation_id")?.as_str()?;
        let id = text.parse::<AllocationId>().ok()?;
        persistence.get_allocation(&id).ok().flatten()
    }

    /// Persist a phase transition on the transfer row (returns the
    /// updated row).
    async fn set_state(&self, transfer: &Transfer, state: TransferState) -> Transfer {
        let mut row = transfer.clone();
        row.state = state;
        let persistence = Arc::clone(&self.persistence);
        let updated = row.clone();
        tokio::task::spawn_blocking(move || persistence.put_transfer(&updated))
            .await
            .expect("transfer state write task panicked")
            .expect("transfer state write must not fail");
        row
    }

    // ── Observed-state helpers ─────────────────────────────────

    /// Whether the workload should be treated as running at the
    /// start of the transfer. `Starting` and a restart-in-flight
    /// (`Stopping` with a running-wanted desired state) both count:
    /// the power queue serializes this transfer's stop behind
    /// whatever is already in flight, and the workload will be
    /// there to stop.
    fn was_running(snapshot: &ObservedSnapshot) -> bool {
        match snapshot.observed {
            ObservedState::Running { .. } | ObservedState::Starting => true,
            ObservedState::Stopping { .. } | ObservedState::Unknown => {
                snapshot.desired.wants_running()
            }
            ObservedState::Offline { .. } => false,
        }
    }

    /// Poll the observed state until it is `Offline` (or the
    /// deadline / cancellation / an error intervenes).
    async fn wait_for_offline(&self, ctx: &JobContext, instance: InstanceId) -> Waited {
        let deadline = tokio::time::Instant::now() + self.policy.stop_wait;
        loop {
            if ctx.is_cancelled() {
                return Waited::Cancelled;
            }
            match self.supervisor.observed(&instance).await {
                Ok(snapshot) => {
                    if let ObservedState::Offline { .. } = snapshot.observed {
                        return Waited::Reached;
                    }
                }
                Err(e) => return Waited::Failed(e.to_string()),
            }
            if tokio::time::Instant::now() >= deadline {
                return Waited::TimedOut;
            }
            tokio::time::sleep(self.policy.poll_interval).await;
        }
    }

    /// Poll the observed state until it is `Running`.
    async fn wait_for_running(&self, ctx: &JobContext, instance: InstanceId) -> Waited {
        let deadline = tokio::time::Instant::now() + self.policy.start_wait;
        loop {
            if ctx.is_cancelled() {
                return Waited::Cancelled;
            }
            match self.supervisor.observed(&instance).await {
                Ok(snapshot) => {
                    if let ObservedState::Running { .. } = snapshot.observed {
                        return Waited::Reached;
                    }
                }
                Err(e) => return Waited::Failed(e.to_string()),
            }
            if tokio::time::Instant::now() >= deadline {
                return Waited::TimedOut;
            }
            tokio::time::sleep(self.policy.poll_interval).await;
        }
    }

    /// The failure-path power restore: bring the instance back to
    /// `Running` when the transfer stopped it and never delivered
    /// its replacement start. Best-effort by design — a restore
    /// failure is reported, not fatal (the instance is still
    /// startable; the operator sees both messages).
    async fn restore_running(&self, instance: InstanceId) -> Result<(), String> {
        match self
            .supervisor
            .request_power(&instance, PowerAction::Start)
            .await
        {
            Err(e) => Err(format!("power restore rejected: {e}")),
            Ok(_) => {
                let deadline = tokio::time::Instant::now() + self.policy.restart_wait;
                loop {
                    match self.supervisor.observed(&instance).await {
                        Ok(snapshot) => {
                            if let ObservedState::Running { .. } = snapshot.observed {
                                return Ok(());
                            }
                        }
                        Err(e) => return Err(format!("power restore observation failed: {e}")),
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Err("power restore timed out waiting for Running".to_owned());
                    }
                    tokio::time::sleep(self.policy.poll_interval).await;
                }
            }
        }
    }

    // ── The unwinding paths ────────────────────────────────────

    /// A failure at any phase: mark the row, restore the power
    /// state when the unwind bookkeeping says to, and answer the
    /// executor with `cleanup_pending` (the transfer tree is
    /// partial state for the cleaner).
    async fn fail(&self, transfer: &Transfer, message: String, unwind: Unwind) -> JobOutcome {
        self.set_state(transfer, TransferState::Failed).await;
        let mut message = message;
        if unwind.wants_restore() {
            if let Err(restore_error) = self.restore_running(transfer.instance_id).await {
                message.push_str(&format!(" (additionally: {restore_error})"));
            }
        }
        JobOutcome::Failed {
            message,
            cleanup_pending: true,
        }
    }

    /// Cancellation observed at a safe point: mark the row, remove
    /// what this handler can remove inline, restore the power
    /// state, and answer `Cancelled`.
    async fn unwind_cancelled(
        &self,
        transfer: &Transfer,
        instance_short: &str,
        unwind: Unwind,
    ) -> JobOutcome {
        self.set_state(transfer, TransferState::Cancelled).await;
        // Inline best-effort cleanup (a `Cancelled` outcome is
        // terminal — the cleaner will not visit this job).
        let _ = self
            .store
            .remove_transfer_tree(instance_short, &transfer.id.to_string());
        if unwind.wants_restore() {
            // Reported, not fatal: the instance is startable either
            // way; the outcome must stay `Cancelled`.
            let _ = self.restore_running(transfer.instance_id).await;
        }
        JobOutcome::Cancelled
    }
}

#[async_trait]
impl JobHandler for TransferJobHandler {
    async fn run(&self, ctx: JobContext) -> JobOutcome {
        let job = ctx.job().clone();

        // ── Resolve the rows this job drives ───────────────────
        let resolved = tokio::task::spawn_blocking({
            let persistence = Arc::clone(&self.persistence);
            let job = job.clone();
            move || {
                let transfer = Self::transfer_for_job_sync(&*persistence, &job)?;
                let instance = persistence.get_instance(&job.instance_id).ok().flatten()?;
                let allocation = Self::allocation_for_job_sync(&*persistence, &job)?;
                Some(Resolved {
                    transfer,
                    instance,
                    allocation,
                })
            }
        })
        .await
        .expect("transfer row resolution task panicked");

        let Some(Resolved {
            transfer,
            instance,
            allocation,
        }) = resolved
        else {
            // No transfer row, no instance, or no landing
            // allocation (claimed or deleted out from under the
            // queued job): nothing this handler has written, so
            // nothing to clean.
            return JobOutcome::Failed {
                message: "the transfer this job operates on can no longer be resolved \
                          (its instance, transfer row, or landing allocation is gone)"
                    .to_owned(),
                cleanup_pending: false,
            };
        };
        let instance_short = instance.uuid_short.clone();
        let live_dir = instance_data_dir(&self.data_root, &instance_short);
        let transfer_id = transfer.id.to_string();
        let mut unwind = Unwind::default();

        // ── SourceStopping ─────────────────────────────────────
        let transfer = self
            .set_state(&transfer, TransferState::SourceStopping)
            .await;
        let snapshot = match self.supervisor.observed(&transfer.instance_id).await {
            Ok(snapshot) => snapshot,
            Err(e) => {
                return JobOutcome::Failed {
                    message: format!("the source instance is not manageable by this engine ({e})"),
                    cleanup_pending: false,
                };
            }
        };
        unwind.was_running = Self::was_running(&snapshot);
        if !unwind.was_running && matches!(snapshot.observed, ObservedState::Unknown) {
            // An unreachable runtime cannot confirm the workload is
            // stopped; refuse to archive a maybe-running workload.
            return JobOutcome::Failed {
                message: "the source workload's state cannot be observed \
                          (runtime unreachable); retry the transfer"
                    .to_owned(),
                cleanup_pending: false,
            };
        }
        if unwind.was_running {
            ctx.progress("stopping", 0.0);
            let stop = PowerAction::Stop {
                deadline: self.policy.stop_deadline,
            };
            if let Err(e) = self
                .supervisor
                .request_power(&transfer.instance_id, stop)
                .await
            {
                return JobOutcome::Failed {
                    message: format!("the source workload could not be stopped: {e}"),
                    cleanup_pending: false,
                };
            }
        }
        match self.wait_for_offline(&ctx, transfer.instance_id).await {
            Waited::Reached => unwind.stopped_for_transfer = unwind.was_running,
            Waited::Cancelled => {
                return self
                    .unwind_cancelled(&transfer, &instance_short, unwind)
                    .await;
            }
            Waited::TimedOut => {
                return self
                    .fail(
                        &transfer,
                        "the source workload did not stop in time".to_owned(),
                        unwind,
                    )
                    .await;
            }
            Waited::Failed(e) => {
                return self
                    .fail(
                        &transfer,
                        format!("observing the source workload failed: {e}"),
                        unwind,
                    )
                    .await;
            }
        }
        ctx.progress("stopping", 1.0);
        if ctx.is_cancelled() {
            return self
                .unwind_cancelled(&transfer, &instance_short, unwind)
                .await;
        }

        // ── SourceArchiving ────────────────────────────────────
        let transfer = self
            .set_state(&transfer, TransferState::SourceArchiving)
            .await;
        let source_archive = self.store.source_archive(&instance_short, &transfer_id);
        {
            // An instance that never started has no data directory
            // yet; it archives as an empty tree.
            let dir = live_dir.clone();
            let _ = tokio::task::spawn_blocking(move || std::fs::create_dir_all(&dir))
                .await
                .expect("data dir create task panicked");
        }
        ctx.progress("archiving", 0.0);
        let cancel = ctx.cancel().clone();
        let progress_fn = ctx.progress_fn();
        let archive_source = live_dir.clone();
        let archive_dest = source_archive.clone();
        let archived = tokio::task::spawn_blocking(move || {
            let is_cancelled = move || cancel.is_cancelled();
            let report = move |fraction: f32| {
                if let Some(report) = &progress_fn {
                    report("archiving", fraction);
                }
            };
            create_archive(&archive_source, &archive_dest, &is_cancelled, &report)
        })
        .await
        .expect("source archive task panicked");
        let archived = match archived {
            Ok(archived) => archived,
            Err(ArchiveError::Cancelled) => {
                return self
                    .unwind_cancelled(&transfer, &instance_short, unwind)
                    .await;
            }
            Err(e) => {
                return self
                    .fail(
                        &transfer,
                        format!("archiving the source data failed: {e}"),
                        unwind,
                    )
                    .await;
            }
        };
        if ctx.is_cancelled() {
            return self
                .unwind_cancelled(&transfer, &instance_short, unwind)
                .await;
        }

        // ── ArchiveTransferring ─────────────────────────────────
        // The node-runtime seam: today a rename between the two
        // halves of the transfer tree plus a digest verification on
        // arrival; tomorrow the bytes cross the wire into the same
        // dest half. The verification is the invariant.
        let transfer = self
            .set_state(&transfer, TransferState::ArchiveTransferring)
            .await;
        let dest_archive = self.store.dest_archive(&instance_short, &transfer_id);
        ctx.progress("transferring", 0.5);
        let ship_source = source_archive.clone();
        let ship_dest = dest_archive.clone();
        let ship = tokio::task::spawn_blocking(move || -> Result<(), ArchiveError> {
            if let Some(parent) = ship_dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| ArchiveError::Io {
                    context: "cannot create the destination archive directory".to_owned(),
                    source: e,
                })?;
            }
            std::fs::rename(&ship_source, &ship_dest).map_err(|e| ArchiveError::Io {
                context: "cannot ship the archive to the destination half".to_owned(),
                source: e,
            })
        })
        .await
        .expect("archive ship task panicked");
        if let Err(e) = ship {
            return self
                .fail(
                    &transfer,
                    format!("shipping the archive failed: {e}"),
                    unwind,
                )
                .await;
        }
        ctx.progress("transferring", 1.0);
        {
            let expected = archived.sha256.clone();
            let verify_dest = dest_archive.clone();
            let verify =
                tokio::task::spawn_blocking(move || verify_archive(&verify_dest, &expected))
                    .await
                    .expect("arrival verification task panicked");
            if let Err(e) = verify {
                return self
                    .fail(
                        &transfer,
                        format!("the shipped archive failed verification: {e}"),
                        unwind,
                    )
                    .await;
            }
        }
        // Record where the archive landed (the row's observable
        // answer to "where are the bytes right now").
        let mut shipped_row = transfer.clone();
        shipped_row.archive_path = Some(dest_archive.display().to_string());
        let transfer = self.set_state(&shipped_row, shipped_row.state).await;
        if ctx.is_cancelled() {
            return self
                .unwind_cancelled(&transfer, &instance_short, unwind)
                .await;
        }

        // ── DestExtracting ──────────────────────────────────────
        let transfer = self
            .set_state(&transfer, TransferState::DestExtracting)
            .await;
        let staging = self.store.staging_dir(&instance_short, &transfer_id);
        ctx.progress("extracting", 0.0);
        let cancel = ctx.cancel().clone();
        let progress_fn = ctx.progress_fn();
        let extract_archive = dest_archive.clone();
        let extract_sha = archived.sha256.clone();
        let extract_staging = staging.clone();
        let extracted = tokio::task::spawn_blocking(move || {
            let is_cancelled = move || cancel.is_cancelled();
            let report = move |fraction: f32| {
                if let Some(report) = &progress_fn {
                    report("extracting", fraction);
                }
            };
            restore_archive(
                &extract_archive,
                &extract_sha,
                &extract_staging,
                &is_cancelled,
                &report,
            )
        })
        .await
        .expect("destination extract task panicked");
        match extracted {
            Err(ArchiveError::Cancelled) => {
                return self
                    .unwind_cancelled(&transfer, &instance_short, unwind)
                    .await;
            }
            Err(e) => {
                return self
                    .fail(
                        &transfer,
                        format!("extracting on the destination failed: {e}"),
                        unwind,
                    )
                    .await;
            }
            Ok(()) => {}
        }

        // ── The commit: swap the verified tree into the live dir ─
        // Not a cancellation point by design (see the module docs).
        let swap_live = live_dir.clone();
        let swap_staging = staging.clone();
        let swapped =
            tokio::task::spawn_blocking(move || swap_contents_in_place(&swap_live, &swap_staging))
                .await
                .expect("destination swap task panicked");
        if let Err(e) = swapped {
            return self
                .fail(
                    &transfer,
                    format!("committing the extracted data failed: {e}"),
                    unwind,
                )
                .await;
        }
        unwind.committed = true;

        // ── Rebind: node + allocation ───────────────────────────
        // The persisted row is written before the start request, so
        // the recreated container binds the destination allocation.
        let instance_id = transfer.instance_id;
        let to_node = transfer.to_node_id;
        let new_allocation = allocation.id;
        let persistence = Arc::clone(&self.persistence);
        let mut rebound = instance.clone();
        rebound.node_id = to_node;
        rebound.allocation_id = new_allocation;
        rebound.updated_at = chrono::Utc::now();
        let old_allocation_id = instance.allocation_id;
        let row = rebound.clone();
        let rebind = tokio::task::spawn_blocking(move || -> Result<(), String> {
            persistence
                .put_instance(&row)
                .map_err(|e| format!("cannot persist the node rebind: {e}"))?;
            // Release the source-side allocation, claim the
            // destination one (the swap updates both sides; a
            // raced-away allocation fails the transfer after the
            // data is already content-equivalent — the instance
            // stays runnable, the operator re-initiates).
            if let Some(previous) = persistence
                .get_allocation(&old_allocation_id)
                .map_err(|e| format!("cannot read the source allocation: {e}"))?
            {
                let mut freed = previous;
                freed.assigned_to = None;
                persistence
                    .put_allocation(&freed)
                    .map_err(|e| format!("cannot release the source allocation: {e}"))?;
            }
            let claimed = persistence
                .get_allocation(&new_allocation)
                .map_err(|e| format!("cannot read the landing allocation: {e}"))?
                .ok_or_else(|| "the landing allocation vanished".to_owned())?;
            if claimed.assigned_to.is_some() && claimed.assigned_to != Some(instance_id) {
                return Err("the landing allocation was claimed by another instance".to_owned());
            }
            let mut claimed = claimed;
            claimed.assigned_to = Some(instance_id);
            persistence
                .put_allocation(&claimed)
                .map_err(|e| format!("cannot claim the landing allocation: {e}"))?;
            Ok(())
        })
        .await
        .expect("rebind task panicked");
        if let Err(message) = rebind {
            return self.fail(&transfer, message, unwind).await;
        }

        // ── DestStarting ────────────────────────────────────────
        let transfer = self.set_state(&transfer, TransferState::DestStarting).await;
        if unwind.was_running {
            ctx.progress("starting", 0.0);
            if let Err(e) = self
                .supervisor
                .request_power(&transfer.instance_id, PowerAction::Start)
                .await
            {
                return self
                    .fail(
                        &transfer,
                        format!("starting the workload on the destination failed: {e}"),
                        unwind,
                    )
                    .await;
            }
            match self.wait_for_running(&ctx, transfer.instance_id).await {
                Waited::Reached => {}
                // The move is committed; finish the bookkeeping (see
                // the module docs) rather than abandoning it.
                Waited::Cancelled => {}
                Waited::TimedOut => {
                    return self
                        .fail(
                            &transfer,
                            "the workload did not reach Running on the destination in time"
                                .to_owned(),
                            unwind,
                        )
                        .await;
                }
                Waited::Failed(e) => {
                    return self
                        .fail(
                            &transfer,
                            format!("observing the destination start failed: {e}"),
                            unwind,
                        )
                        .await;
                }
            }
            ctx.progress("starting", 1.0);
        }

        // ── SourceCleanup ───────────────────────────────────────
        let transfer = self
            .set_state(&transfer, TransferState::SourceCleanup)
            .await;
        ctx.progress("cleaning", 1.0);
        {
            let store = self.store.clone();
            let short = instance_short.clone();
            let id = transfer_id.clone();
            let cleaned =
                tokio::task::spawn_blocking(move || store.remove_transfer_tree(&short, &id))
                    .await
                    .expect("source cleanup task panicked");
            if let Err(e) = cleaned {
                return self
                    .fail(
                        &transfer,
                        format!("cleaning the transfer tree failed: {e}"),
                        unwind,
                    )
                    .await;
            }
            self.store.prune_instance_dir(&instance_short);
        }

        // ── Succeeded ───────────────────────────────────────────
        self.set_state(&transfer, TransferState::Succeeded).await;
        JobOutcome::Succeeded
    }

    async fn cleanup(&self, job: &Job) -> Result<(), String> {
        // Everything this handler can leave behind is derived from
        // the instance row (its uuid_short keys the tree) and the
        // job's payload (the transfer id keys the directory inside
        // it). Both are resolvable from the persisted rows; a
        // missing row means nothing derivable remains (mirroring
        // the backup handler's honesty about unreachable paths).
        let instance = tokio::task::spawn_blocking({
            let persistence = Arc::clone(&self.persistence);
            let instance_id = job.instance_id;
            move || persistence.get_instance(&instance_id).ok().flatten()
        })
        .await
        .expect("cleanup instance lookup task panicked");
        let Some(instance) = instance else {
            return Ok(());
        };

        let transfer = tokio::task::spawn_blocking({
            let persistence = Arc::clone(&self.persistence);
            let job = job.clone();
            move || Self::transfer_for_job_sync(&*persistence, &job)
        })
        .await
        .expect("cleanup transfer lookup task panicked");
        let Some(transfer) = transfer else {
            // No row, no derivable tree (the payload keyed it).
            return Ok(());
        };

        self.store
            .remove_transfer_tree(&instance.uuid_short, &transfer.id.to_string())?;
        self.store.prune_instance_dir(&instance.uuid_short);
        Ok(())
    }
}
