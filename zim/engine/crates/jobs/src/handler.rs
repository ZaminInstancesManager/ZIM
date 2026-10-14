//! The job-handler boundary: what the executor gives a job, and
//! what a job hands back.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::watch;

use zim_engine_domain::job::Job;

/// Cooperative cancellation token handed to a running handler.
///
/// The executor sets it when [`JobSystem::cancel`] is called for an
/// `InProgress` job (and at shutdown, where awaiting the handler is
/// part of the drain). Handlers should poll
/// [`CancelToken::is_cancelled`] between long steps and, ideally,
/// `select!` on [`CancelToken::cancelled`] inside their inner loops.
///
/// The token holds a receiver clone of its own watch channel, so
/// the flag remains readable (and settable) no matter how many
/// external receivers come and go.
#[derive(Clone)]
pub struct CancelToken {
    tx: Arc<watch::Sender<bool>>,
    rx: watch::Receiver<bool>,
}

impl CancelToken {
    pub(crate) fn new() -> Self {
        let (tx, rx) = watch::channel(false);
        Self {
            tx: Arc::new(tx),
            rx,
        }
    }

    /// Fire the cancellation signal (idempotent).
    pub(crate) fn cancel(&self) {
        // The token itself keeps a receiver alive, so the value is
        // always stored; a send error here would still be harmless.
        let _ = self.tx.send(true);
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        *self.rx.borrow()
    }

    /// Resolves when cancellation is requested (also resolves
    /// immediately if it already was).
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let mut rx = self.rx.clone();
        // Watch receivers yield the *current* value first; loop until
        // the observed value is the cancellation flag.
        loop {
            if *rx.borrow() {
                return;
            }
            if rx.changed().await.is_err() {
                // Sender dropped without cancelling: treat as done.
                return;
            }
        }
    }
}

/// A progress-report callback: `(stage, fraction)` where `stage` is
/// a stable snake_case stage name and `fraction` is 0.0–1.0.
///
/// The executor installs one that publishes a `job.progress` event
/// onto the streaming hub; tests install recorders.
pub type ProgressFn = Arc<dyn Fn(&str, f32) + Send + Sync>;

/// Everything a running handler needs: a snapshot of its job, a
/// cancellation token, and the progress reporter.
///
/// The snapshot is taken at claim time; handlers that need fresher
/// job data (rare) re-read through their own dependencies.
pub struct JobContext {
    job: Job,
    cancel: CancelToken,
    progress: Option<ProgressFn>,
}

impl JobContext {
    pub(crate) fn new(job: Job, cancel: CancelToken, progress: Option<ProgressFn>) -> Self {
        Self {
            job,
            cancel,
            progress,
        }
    }

    /// The job being executed (snapshot from claim time).
    pub fn job(&self) -> &Job {
        &self.job
    }

    /// The job's id (convenience).
    pub fn job_id(&self) -> zim_engine_domain::JobId {
        self.job.id
    }

    /// The instance the job operates on (convenience).
    pub fn instance_id(&self) -> zim_engine_domain::InstanceId {
        self.job.instance_id
    }

    /// The cancellation token.
    pub fn cancel(&self) -> &CancelToken {
        &self.cancel
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Resolves when cancellation is requested.
    pub async fn cancelled(&self) {
        self.cancel.cancelled().await
    }

    /// Report progress for a named stage (fraction 0.0–1.0).
    ///
    /// Out-of-range fractions are clamped; unknown stages are the
    /// handler's documentation problem, not the executor's.
    pub fn progress(&self, stage: &str, fraction: f32) {
        let fraction = fraction.clamp(0.0, 1.0);
        if let Some(report) = &self.progress {
            report(stage, fraction);
        }
    }

    /// The installed progress reporter (for handlers that shuttle
    /// reporting into `spawn_blocking` closures — the `Arc` clones).
    pub fn progress_fn(&self) -> Option<ProgressFn> {
        self.progress.as_ref().map(Arc::clone)
    }
}

/// What a handler reports when its `run` future finishes.
///
/// The mapping into persisted [`zim_engine_domain::status::JobState`]
/// is the executor's job:
///
/// - `Succeeded` → `Succeeded`
/// - `Cancelled` → `Cancelled`
/// - `Failed { cleanup_pending: false }` → `Failed` (terminal)
/// - `Failed { cleanup_pending: true }` → `CleanupPending`
///   (non-terminal; the background cleaner invokes
///   [`JobHandler::cleanup`], then the job lands in `Failed`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobOutcome {
    /// The job finished successfully.
    Succeeded,
    /// The handler observed cancellation and stopped in a state it
    /// considers consistent (no partial state left behind).
    Cancelled,
    /// The job failed.
    Failed {
        /// Human-readable failure message (persisted as
        /// `last_error`).
        message: String,
        /// Whether the failure left partial on-disk state that the
        /// background cleaner must remove (invariant 11).
        cleanup_pending: bool,
    },
}

/// A durable job's executable half.
///
/// One implementation per [`zim_engine_domain::job::JobKind`],
/// registered on the [`crate::JobSystem`]. `run` executes the job
/// from its persisted snapshot; `cleanup` removes whatever partial
/// state a failed run may have left (a partial archive, a staging
/// directory, ...) — it must be idempotent and must succeed when
/// nothing is left to clean ("nothing to do" is success).
#[async_trait]
pub trait JobHandler: Send + Sync {
    /// Execute the job. Long-running loops should honor
    /// [`JobContext::cancelled`]; returning
    /// [`JobOutcome::Cancelled`] after observing it keeps the
    /// persisted state machine honest.
    async fn run(&self, ctx: JobContext) -> JobOutcome;

    /// Remove the partial state a failed run of `job` left behind.
    ///
    /// Called by the background cleaner for jobs in
    /// `CleanupPending`, including jobs orphaned by an engine crash
    /// (boot reconciliation moves those to `CleanupPending`
    /// precisely so this runs). The `job` argument is the persisted
    /// row at failure time — derive the partial-state location from
    /// it the same way `run` derived the live one.
    async fn cleanup(&self, job: &Job) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancel_token_cancelled_resolves_after_cancel() {
        let token = CancelToken::new();
        assert!(!token.is_cancelled());
        token.cancel();
        assert!(token.is_cancelled());
        // Already cancelled: returns immediately.
        token.cancelled().await;
    }

    #[tokio::test]
    async fn cancel_token_cancelled_waits_for_the_flip() {
        let token = CancelToken::new();
        let waiter = tokio::spawn({
            let token = token.clone();
            async move { token.cancelled().await }
        });
        // Give the waiter a chance to park (it must not have
        // resolved yet).
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(!waiter.is_finished());
        token.cancel();
        waiter.await.unwrap();
    }

    #[tokio::test]
    async fn cancel_token_survives_dropped_receivers() {
        let token = CancelToken::new();
        // A clone that goes away does not affect the flag.
        {
            let _clone = token.clone();
        }
        token.cancel();
        assert!(token.is_cancelled());
        token.cancelled().await;
    }

    #[test]
    fn context_reports_clamped_progress() {
        let recorded: Arc<std::sync::Mutex<Vec<(String, f32)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&recorded);
        let recorder: ProgressFn = Arc::new(move |stage, fraction| {
            sink.lock().unwrap().push((stage.to_owned(), fraction));
        });

        let job = zim_engine_domain::job::Job {
            id: zim_engine_domain::JobId::new(),
            kind: zim_engine_domain::job::JobKind::Backup,
            instance_id: zim_engine_domain::InstanceId::new(),
            state: zim_engine_domain::status::JobState::InProgress,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            last_error: None,
            cleanup_pending: false,
            generation: 0,
            payload: None,
        };
        let token = CancelToken::new();
        let ctx = JobContext::new(job, token, Some(recorder));

        ctx.progress("archiving", 2.0);
        ctx.progress("hashing", -1.0);
        ctx.progress("sealing", 0.5);

        assert_eq!(
            *recorded.lock().unwrap(),
            vec![
                ("archiving".to_owned(), 1.0),
                ("hashing".to_owned(), 0.0),
                ("sealing".to_owned(), 0.5),
            ]
        );
    }

    #[test]
    fn context_without_reporter_is_a_noop() {
        let job = zim_engine_domain::job::Job {
            id: zim_engine_domain::JobId::new(),
            kind: zim_engine_domain::job::JobKind::Backup,
            instance_id: zim_engine_domain::InstanceId::new(),
            state: zim_engine_domain::status::JobState::InProgress,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            last_error: None,
            cleanup_pending: false,
            generation: 0,
            payload: None,
        };
        let token = CancelToken::new();
        let ctx = JobContext::new(job, token, None);
        // No reporter installed: a no-op, not a panic.
        ctx.progress("archiving", 0.5);
        assert_eq!(
            ctx.job().state,
            zim_engine_domain::status::JobState::InProgress
        );
    }
}
