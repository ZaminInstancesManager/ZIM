//! The durable-job executor.
//!
//! [`JobSystem`] owns the lifetime of every job row: it claims
//! `Pending` jobs (persisting `Pending → InProgress` before any side
//! effect), runs the registered [`JobHandler`] for the job's kind
//! under a per-instance serialization lock and a global concurrency
//! cap, persists the terminal transition, and publishes
//! `job.progress` / `job.completed` events onto the streaming hub.
//!
//! Crash safety follows `docs/architecture/RUST_ENGINE.md` §8: a job
//! that was `InProgress` when the process died is an orphan —
//! [`JobSystem::reconcile_orphans`] (called at boot, before
//! [`JobSystem::start`]) moves it to `CleanupPending` so the
//! background cleaner ([`JobSystem::clean_once`]) removes the partial
//! on-disk state and the job lands in `Failed`. `Pending` jobs were
//! never claimed: the executor simply picks them up.

use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt;
use tokio::sync::{watch, RwLock};
use tokio::time::sleep;

use zim_engine_application::ports::Persistence;
use zim_engine_domain::job::{Job, JobKind};
use zim_engine_domain::status::JobState;
use zim_engine_domain::{InstanceId, JobId};
use zim_engine_streaming::{EventData, StreamHub};

use crate::handler::{CancelToken, JobContext, JobOutcome};

/// Executor tuning knobs.
#[derive(Debug, Clone)]
pub struct JobPolicy {
    /// How often the queue re-scans for `Pending` jobs even without
    /// an enqueue notification (crash-recovered rows, manual DB
    /// edits, missed notifications).
    pub poll_interval: Duration,
    /// How often the background cleaner scans for `CleanupPending`
    /// jobs.
    pub cleaner_interval: Duration,
    /// Maximum jobs executing simultaneously (across all instances).
    pub max_concurrent: usize,
    /// How long [`JobSystem::shutdown`] waits for running jobs to
    /// finish before returning (they keep running detached; the next
    /// boot's reconciliation handles the stragglers).
    pub shutdown_grace: Duration,
}

impl Default for JobPolicy {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(500),
            cleaner_interval: Duration::from_secs(30),
            max_concurrent: 2,
            shutdown_grace: Duration::from_secs(30),
        }
    }
}

/// Errors from the job system's own operations (not from handlers —
/// those surface as `Failed`/`CleanupPending` job rows).
#[derive(Debug, thiserror::Error)]
pub enum JobsError {
    /// The persistence store failed.
    #[error("persistence: {0}")]
    Persistence(#[from] zim_engine_application::ports::PersistenceError),
    /// The job does not exist.
    #[error("job {0} not found")]
    NotFound(JobId),
    /// The job is already in a state that cannot be cancelled
    /// (terminal, or owned by the cleaner).
    #[error("job {0} is in state {1:?}, which cannot be cancelled")]
    Uncancellable(JobId, JobState),
    /// No handler is registered for the job's kind.
    #[error("no handler registered for job kind {kind:?} (job {job})")]
    NoHandler {
        /// The kind with no registered handler.
        kind: JobKind,
        /// The job that needed it.
        job: JobId,
    },
}

/// What happened when [`JobSystem::cancel`] was called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelStatus {
    /// The job was `Pending` (or claimed-but-never-started): it
    /// moved straight to a terminal state.
    Cancelled,
    /// The job was `InProgress`: the cancel signal was delivered; the
    /// running handler observes it and finishes shortly.
    Cancelling,
}

/// A job currently executing.
struct RunningJob {
    instance: InstanceId,
    cancel: CancelToken,
}

/// The durable-job executor (see the module docs).
pub struct JobSystem {
    persistence: Arc<dyn Persistence>,
    hub: Option<StreamHub>,
    policy: JobPolicy,
    handlers: RwLock<HashMap<JobKind, Arc<dyn crate::handler::JobHandler>>>,
    queue_wake: tokio::sync::Notify,
    cleaner_wake: tokio::sync::Notify,
    shutdown_tx: watch::Sender<bool>,
    running: tokio::sync::Mutex<HashMap<JobId, RunningJob>>,
}

impl JobSystem {
    /// Build an executor over `persistence`, publishing job events to
    /// `hub` (when present; `None` is for embedders that don't want
    /// events). Call [`JobSystem::reconcile_orphans`] once at boot,
    /// then [`JobSystem::start`].
    pub fn new(
        persistence: Arc<dyn Persistence>,
        hub: Option<StreamHub>,
        policy: JobPolicy,
    ) -> Arc<Self> {
        let (shutdown_tx, _) = watch::channel(false);
        Arc::new(Self {
            persistence,
            hub,
            policy,
            handlers: RwLock::new(HashMap::new()),
            queue_wake: tokio::sync::Notify::new(),
            cleaner_wake: tokio::sync::Notify::new(),
            shutdown_tx,
            running: tokio::sync::Mutex::new(HashMap::new()),
        })
    }

    /// The policy in effect (observability).
    pub fn policy(&self) -> &JobPolicy {
        &self.policy
    }

    /// Register the handler for a job kind (idempotent replace).
    ///
    /// Must happen before jobs of that kind are claimed; a claimed
    /// job with no handler fails with a `no handler registered`
    /// `last_error`.
    pub async fn register_handler(
        &self,
        kind: JobKind,
        handler: Arc<dyn crate::handler::JobHandler>,
    ) {
        self.handlers.write().await.insert(kind, handler);
    }

    /// The handler registered for `kind` (cleaner + tests).
    pub async fn handler_for(&self, kind: JobKind) -> Option<Arc<dyn crate::handler::JobHandler>> {
        self.handlers.read().await.get(&kind).cloned()
    }

    /// Persist a new (typically `Pending`) job and wake the queue.
    ///
    /// The row is written exactly as given — callers construct the
    /// `Job` (id, kind, instance, timestamps, generation). Jobs
    /// enqueued in any non-`Pending` state are simply persisted and
    /// never claimed.
    pub async fn enqueue(&self, job: Job) -> Result<Job, JobsError> {
        let persistence = Arc::clone(&self.persistence);
        let row = job.clone();
        tokio::task::spawn_blocking(move || persistence.put_job(&row))
            .await
            .expect("enqueue task panicked")?;
        self.queue_wake.notify_one();
        Ok(job)
    }

    /// Boot-time crash reconciliation (RUST_ENGINE.md §8).
    ///
    /// Every `InProgress` job belongs to a dead process — this
    /// executor claims jobs by persisting `InProgress` itself, so a
    /// row in that state at boot predates us. Each orphan moves to
    /// `CleanupPending` with `cleanup_pending = true` so the
    /// background cleaner removes its partial state; `Pending` rows
    /// survive untouched (the queue picks them up). Returns the
    /// number of orphaned jobs.
    pub async fn reconcile_orphans(&self) -> Result<usize, JobsError> {
        let persistence = Arc::clone(&self.persistence);
        let orphans = tokio::task::spawn_blocking(move || {
            persistence.list_jobs_by_state(JobState::InProgress)
        })
        .await
        .expect("reconcile task panicked")?;
        let count = orphans.len();

        let now = chrono::Utc::now();
        for mut job in orphans {
            job.state = JobState::CleanupPending;
            job.cleanup_pending = true;
            job.completed_at = Some(now);
            job.updated_at = now;
            job.last_error = Some(format!(
                "engine restarted while the job was executing (orphaned {:?} job)",
                job.kind
            ));
            let persistence = Arc::clone(&self.persistence);
            let row = job;
            tokio::task::spawn_blocking(move || persistence.put_job(&row))
                .await
                .expect("reconcile write task panicked")?;
        }
        // Freshly-reconciled orphans are immediately cleanable.
        self.cleaner_wake.notify_one();
        Ok(count)
    }

    /// Spawn the queue and cleaner loops. Call once per process,
    /// after all handlers are registered.
    pub fn start(self: &Arc<Self>) {
        let queue = Arc::clone(self);
        tokio::spawn(async move { queue.queue_loop().await });
        let cleaner = Arc::clone(self);
        tokio::spawn(async move { cleaner.cleaner_loop().await });
    }

    /// Request cancellation of a job.
    ///
    /// `Pending` jobs cancel immediately (straight to `Cancelled`).
    /// `InProgress` jobs get the cooperative signal; the handler
    /// finishes on its own terms (the persisted state follows its
    /// [`JobOutcome`]). Terminal and cleaner-owned jobs answer
    /// [`JobsError::Uncancellable`].
    pub async fn cancel(&self, id: JobId) -> Result<CancelStatus, JobsError> {
        let persistence = Arc::clone(&self.persistence);
        let job = tokio::task::spawn_blocking(move || persistence.get_job(&id))
            .await
            .expect("cancel read task panicked")?
            .ok_or(JobsError::NotFound(id))?;

        if job.state.is_terminal() || job.state == JobState::CleanupPending {
            return Err(JobsError::Uncancellable(id, job.state));
        }

        if job.state == JobState::Pending {
            let instance = job.instance_id;
            let mut cancelled = job;
            cancelled.state = JobState::Cancelled;
            cancelled.completed_at = Some(chrono::Utc::now());
            cancelled.updated_at = cancelled.completed_at.unwrap();
            cancelled.last_error = None;
            cancelled.cleanup_pending = false;
            let persistence = Arc::clone(&self.persistence);
            let row = cancelled;
            tokio::task::spawn_blocking(move || persistence.put_job(&row))
                .await
                .expect("cancel write task panicked")?;
            self.publish_completed(&instance, id, false);
            return Ok(CancelStatus::Cancelled);
        }

        // InProgress: deliver the cooperative signal.
        let token = {
            let running = self.running.lock().await;
            running.get(&id).map(|r| r.cancel.clone())
        };
        match token {
            Some(token) => {
                token.cancel();
                Ok(CancelStatus::Cancelling)
            }
            None => {
                // A claimed row without a live task: the claiming
                // process died between the state write and the spawn
                // (or the task already finished and is writing its
                // terminal row). Finish it now — nothing partial can
                // exist, because the handler never started under us.
                let mut failed = job;
                failed.state = JobState::Failed;
                failed.completed_at = Some(chrono::Utc::now());
                failed.updated_at = failed.completed_at.unwrap();
                failed.last_error =
                    Some("job was claimed but never started (engine crash window)".to_owned());
                failed.cleanup_pending = false;
                let instance = failed.instance_id;
                let persistence = Arc::clone(&self.persistence);
                let row = failed;
                tokio::task::spawn_blocking(move || persistence.put_job(&row))
                    .await
                    .expect("cancel claimed task panicked")?;
                self.publish_completed(&instance, id, false);
                Ok(CancelStatus::Cancelled)
            }
        }
    }

    /// Stop the loops and wait (up to the policy's grace) for running
    /// jobs. Grace-exceeded jobs keep running detached; the next
    /// boot's reconciliation turns them into cleaned-up failures.
    /// Queued `Pending` jobs survive to the next boot by design.
    pub async fn shutdown(&self) {
        // Idempotent: a second send is harmless.
        let _ = self.shutdown_tx.send(true);
        self.queue_wake.notify_one();
        self.cleaner_wake.notify_one();

        let deadline = tokio::time::Instant::now() + self.policy.shutdown_grace;
        loop {
            if self.running.lock().await.is_empty() {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                let still_running = self.running.lock().await.len();
                eprintln!(
                    "zim_engine_jobs: shutdown grace expired with {still_running} job(s) \
                     still running; they will be reconciled on the next boot"
                );
                return;
            }
            sleep(Duration::from_millis(50)).await;
        }
    }

    // ── Queue ───────────────────────────────────────────────────

    /// The queue loop: wait for a wake signal or the poll tick, then
    /// claim and spawn every claimable `Pending` job.
    async fn queue_loop(self: Arc<Self>) {
        let mut shutdown = self.shutdown_tx.subscribe();
        loop {
            tokio::select! {
                _ = self.queue_wake.notified() => {},
                _ = sleep(self.policy.poll_interval) => {},
                _ = shutdown.changed() => {
                    if *shutdown.borrow() { break; }
                },
            }
            if let Err(e) = self.claim_pending().await {
                eprintln!("zim_engine_jobs: queue scan failed: {e}");
            }
        }
    }

    /// Claim and spawn claimable `Pending` jobs (oldest first).
    ///
    /// Claimability: a free concurrency slot, no other running job
    /// for the same instance, and a registered handler.
    async fn claim_pending(self: &Arc<Self>) -> Result<(), JobsError> {
        let persistence = Arc::clone(&self.persistence);
        let pending =
            tokio::task::spawn_blocking(move || persistence.list_jobs_by_state(JobState::Pending))
                .await
                .expect("queue read task panicked")?;

        let mut queue: Vec<Job> = pending;
        queue.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.0.cmp(&b.id.0))
        });

        for job in queue {
            let handler = match self.handler_for(job.kind).await {
                Some(handler) => handler,
                None => {
                    // Fail fast and loudly: a registered-kind gap is
                    // a composition bug, not a retryable condition.
                    let message = format!("no handler registered for job kind {:?}", job.kind);
                    self.fail_job(&job, &message, false).await?;
                    continue;
                }
            };

            let claimable = {
                let mut running = self.running.lock().await;
                if running.len() >= self.policy.max_concurrent {
                    break;
                }
                if running.values().any(|r| r.instance == job.instance_id) {
                    continue;
                }
                // One token, two owners: the map keeps one (for
                // `cancel`), the task carries a clone into the
                // handler's context.
                let token = CancelToken::new();
                let task_token = token.clone();
                let system = Arc::clone(self);
                let task_handler = Arc::clone(&handler);
                let claimed_job = job.clone();
                tokio::spawn(async move {
                    system.execute(task_handler, claimed_job, task_token).await;
                });
                running.insert(
                    job.id,
                    RunningJob {
                        instance: job.instance_id,
                        cancel: token,
                    },
                );
                true
            };
            let _ = claimable;
        }
        Ok(())
    }

    /// Execute one claimed job to its terminal state and remove it
    /// from the running set.
    ///
    /// Owns everything the task needs so no caller ever holds a lock
    /// across the handler await.
    async fn execute(
        self: Arc<Self>,
        handler: Arc<dyn crate::handler::JobHandler>,
        mut job: Job,
        cancel: CancelToken,
    ) {
        // 1. Claim: persist InProgress + started_at BEFORE any side
        //    effect (a crash between here and the handler leaves a
        //    reconcilable orphan).
        job.state = JobState::InProgress;
        job.started_at = Some(chrono::Utc::now());
        job.updated_at = job.started_at.unwrap();
        if let Err(e) = tokio::task::spawn_blocking({
            let persistence = Arc::clone(&self.persistence);
            let row = job.clone();
            move || persistence.put_job(&row)
        })
        .await
        .expect("claim task panicked")
        {
            eprintln!(
                "zim_engine_jobs: cannot claim job {} ({}); leaving it Pending",
                job.id, e
            );
            self.running.lock().await.remove(&job.id);
            return;
        }

        // 2. Run the handler with a progress reporter wired to the
        //    hub's job.progress event. A panicking handler becomes a
        //    cleanup-pending failure: its partial state is unknown,
        //    so assume the worst and let the cleaner decide.
        let progress = self.hub.as_ref().map(|hub| {
            let hub = hub.clone();
            let instance = job.instance_id;
            let job_id = job.id;
            let progress: crate::handler::ProgressFn = Arc::new(move |stage, fraction| {
                hub.publish(
                    instance,
                    EventData::JobProgress {
                        job_id,
                        stage: stage.to_owned(),
                        progress: fraction,
                    },
                );
            });
            progress
        });
        let ctx = JobContext::new(job.clone(), cancel, progress);
        let outcome = AssertUnwindSafe(handler.run(ctx))
            .catch_unwind()
            .await
            .unwrap_or_else(|panic| {
                let message = match panic.downcast_ref::<&str>() {
                    Some(s) => (*s).to_owned(),
                    None => match panic.downcast_ref::<String>() {
                        Some(s) => s.clone(),
                        None => "handler panicked".to_owned(),
                    },
                };
                JobOutcome::Failed {
                    message: format!("handler panicked: {message}"),
                    cleanup_pending: true,
                }
            });

        // 3. Persist the terminal transition, then release the slot
        //    (release first: the next job for this instance may be
        //    claimed while the row write completes).
        let now = chrono::Utc::now();
        let (final_state, success, message, cleanup_pending) = match outcome {
            JobOutcome::Succeeded => (JobState::Succeeded, true, None, false),
            JobOutcome::Cancelled => (JobState::Cancelled, false, None, false),
            JobOutcome::Failed {
                message,
                cleanup_pending,
            } => {
                let state = if cleanup_pending {
                    JobState::CleanupPending
                } else {
                    JobState::Failed
                };
                (state, false, Some(message), cleanup_pending)
            }
        };
        job.state = final_state;
        job.completed_at = Some(now);
        job.updated_at = now;
        job.last_error = message;
        job.cleanup_pending = cleanup_pending;

        self.running.lock().await.remove(&job.id);

        if let Err(e) = tokio::task::spawn_blocking({
            let persistence = Arc::clone(&self.persistence);
            let row = job.clone();
            move || persistence.put_job(&row)
        })
        .await
        .expect("terminal write task panicked")
        {
            eprintln!(
                "zim_engine_jobs: CANNOT PERSIST terminal state for job {} ({}); the row \
                 says InProgress and the next boot will reconcile it",
                job.id, e
            );
            return;
        }

        // 4. Announce completion; freshly-dirty jobs wake the cleaner.
        if let Some(hub) = &self.hub {
            hub.publish(
                job.instance_id,
                EventData::JobCompleted {
                    job_id: job.id,
                    success,
                },
            );
        }
        if cleanup_pending {
            self.cleaner_wake.notify_one();
        }
    }

    /// Move a job straight to a failed state (no handler ran).
    async fn fail_job(
        &self,
        job: &Job,
        message: &str,
        cleanup_pending: bool,
    ) -> Result<(), JobsError> {
        let mut failed = job.clone();
        failed.state = if cleanup_pending {
            JobState::CleanupPending
        } else {
            JobState::Failed
        };
        failed.cleanup_pending = cleanup_pending;
        failed.completed_at = Some(chrono::Utc::now());
        failed.updated_at = failed.completed_at.unwrap();
        failed.last_error = Some(message.to_owned());
        let instance = job.instance_id;
        let persistence = Arc::clone(&self.persistence);
        let row = failed;
        tokio::task::spawn_blocking(move || persistence.put_job(&row))
            .await
            .expect("fail task panicked")?;
        self.publish_completed(&instance, job.id, false);
        if cleanup_pending {
            self.cleaner_wake.notify_one();
        }
        Ok(())
    }

    // ── Cleaner ─────────────────────────────────────────────────

    /// The cleaner loop: every `cleaner_interval` (or on demand)
    /// clean every `CleanupPending` job whose partial state can be
    /// removed, then land it in terminal `Failed`.
    async fn cleaner_loop(self: Arc<Self>) {
        let mut shutdown = self.shutdown_tx.subscribe();
        loop {
            tokio::select! {
                _ = self.cleaner_wake.notified() => {},
                _ = sleep(self.policy.cleaner_interval) => {},
                _ = shutdown.changed() => {
                    if *shutdown.borrow() { break; }
                },
            }
            if let Err(e) = self.clean_once().await {
                eprintln!("zim_engine_jobs: cleaner scan failed: {e}");
            }
        }
    }

    /// One cleaner pass (also the seam the tests drive directly).
    ///
    /// A job lands in terminal `Failed` when its handler's cleanup
    /// succeeds ("nothing to clean" is success); a failing cleanup
    /// retries on the next pass with the failure recorded in
    /// `last_error`.
    pub async fn clean_once(&self) -> Result<usize, JobsError> {
        let persistence = Arc::clone(&self.persistence);
        let dirty = tokio::task::spawn_blocking(move || {
            persistence.list_jobs_by_state(JobState::CleanupPending)
        })
        .await
        .expect("cleaner read task panicked")?;

        let mut cleaned = 0;
        for job in dirty {
            let handler = match self.handler_for(job.kind).await {
                Some(handler) => handler,
                None => {
                    // No handler means nothing kind-specific can be
                    // cleaned; land the job in Failed so it does not
                    // retry forever.
                    let message = format!(
                        "no handler registered to clean up {:?} job {}",
                        job.kind, job.id
                    );
                    self.finish_cleaned(&job, &message).await?;
                    cleaned += 1;
                    continue;
                }
            };
            match handler.cleanup(&job).await {
                Ok(()) => {
                    let message = job.last_error.as_deref().unwrap_or("").to_owned();
                    self.finish_cleaned(&job, &message).await?;
                    cleaned += 1;
                }
                Err(e) => {
                    // Retry next pass; surface the reason on the row.
                    let mut retrying = job.clone();
                    retrying.updated_at = chrono::Utc::now();
                    retrying.last_error = Some(format!("cleanup failed (will retry): {e}"));
                    let persistence = Arc::clone(&self.persistence);
                    let row = retrying;
                    if let Err(e) = tokio::task::spawn_blocking(move || persistence.put_job(&row))
                        .await
                        .expect("cleaner retry task panicked")
                    {
                        eprintln!(
                            "zim_engine_jobs: cannot record cleanup retry for job {}: {e}",
                            job.id
                        );
                    }
                }
            }
        }
        Ok(cleaned)
    }

    /// A `CleanupPending` job whose partial state is gone: terminal
    /// `Failed`, `cleanup_pending` cleared, `completed_at` sealed.
    async fn finish_cleaned(&self, job: &Job, message: &str) -> Result<(), JobsError> {
        let mut failed = job.clone();
        failed.state = JobState::Failed;
        failed.cleanup_pending = false;
        failed.completed_at = Some(chrono::Utc::now());
        failed.updated_at = failed.completed_at.unwrap();
        failed.last_error = if message.is_empty() {
            Some("job failed; partial state cleaned up".to_owned())
        } else {
            Some(format!("{message} (partial state cleaned up)"))
        };
        let instance = job.instance_id;
        let persistence = Arc::clone(&self.persistence);
        let row = failed;
        tokio::task::spawn_blocking(move || persistence.put_job(&row))
            .await
            .expect("finish-cleaned task panicked")?;
        self.publish_completed(&instance, job.id, false);
        Ok(())
    }

    // ── Observability ───────────────────────────────────────────

    fn publish_completed(&self, instance: &InstanceId, job: JobId, success: bool) {
        if let Some(hub) = &self.hub {
            hub.publish(
                *instance,
                EventData::JobCompleted {
                    job_id: job,
                    success,
                },
            );
        }
    }

    /// How many jobs are executing right now.
    pub async fn running_count(&self) -> usize {
        self.running.lock().await.len()
    }

    /// The instances with a job executing right now (tests).
    pub async fn running_instances(&self) -> HashSet<InstanceId> {
        self.running
            .lock()
            .await
            .values()
            .map(|r| r.instance)
            .collect()
    }

    /// Whether the executor's loops have been asked to stop.
    pub fn is_shutting_down(&self) -> bool {
        *self.shutdown_tx.borrow()
    }
}
