//! The executable half of the install system: the
//! `JobKind::Install` / `JobKind::Update` handler.
//!
//! One handler serves both kinds, discriminating on the job's
//! `kind` (plus the payload's skip flag):
//!
//! - **Install** — the egg's `scripts.install`, verbatim.
//! - **Update** — the egg's `scripts.update` when present, falling
//!   back to `scripts.install` (Pterodactyl's egg-change reinstall:
//!   an egg without a dedicated update script is reinstalled).
//!
//! The pipeline (roadmap §M8's `Pending → InProgress(stage=
//! PullingImage) → InProgress(stage=RunningInstallScript) →
//! Succeeded`):
//!
//! ```text
//! (stop the workload if running) → pull the installer image →
//! run the script in the constrained container → stamp
//! installed_at → Succeeded
//! ```
//!
//! # Failure semantics
//!
//! A failed script (non-zero exit, timeout, backend error) fails
//! the job with the exit code and output tail in `last_error`,
//! `cleanup_pending = false` — the partial downloads in the data
//! directory are the *retry state* the next install builds on, not
//! garbage for the cleaner (the installer container itself is
//! removed by the runner on every exit path). The workload stays
//! stopped (it was stopped for the install; the operator starts
//! it once the failure is addressed).
//!
//! # Cancellation
//!
//! Honored through the port's cooperative hook: the runner kills
//! and removes the container the moment the token flips, and this
//! handler answers `Cancelled`. The data directory keeps whatever
//! the script wrote before the kill (same retry-state reasoning).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;

use zim_engine_application::ports::Persistence;
use zim_engine_application::supervision::{
    instance_data_dir, InstallCancelledFn, InstallProgressFn, InstallRunError, InstallRunOutcome,
    InstallRunner, InstallScriptSpec, ObservedState, PowerAction, Supervisor,
};
use zim_engine_domain::job::{Job, JobKind};
use zim_engine_domain::{Egg, EggScript, Instance, InstanceId};

use zim_engine_jobs::{JobContext, JobHandler, JobOutcome};

/// Phase timing knobs. The defaults bound the whole pipeline well
/// under the executor's shutdown grace; tests tighten them.
#[derive(Debug, Clone)]
pub struct InstallPhasePolicy {
    /// How long a graceful stop may take before the power worker
    /// kills the workload (handed to `PowerAction::Stop`).
    pub stop_deadline: Duration,
    /// How long the handler waits for the observed state to reach
    /// `Offline` after requesting (or observing) a stop.
    pub stop_wait: Duration,
    /// How long the script may run before the runner kills its
    /// container (Wings leaves this unbounded; ZIM bounds every
    /// untrusted script run by default).
    pub install_timeout: Duration,
    /// The observed-state poll cadence.
    pub poll_interval: Duration,
}

impl Default for InstallPhasePolicy {
    fn default() -> Self {
        Self {
            stop_deadline: Duration::from_secs(30),
            stop_wait: Duration::from_secs(90),
            install_timeout: Duration::from_secs(900),
            poll_interval: Duration::from_millis(200),
        }
    }
}

/// The install/update job handler (see the module docs).
pub struct InstallJobHandler {
    persistence: Arc<dyn Persistence>,
    supervisor: Arc<Supervisor>,
    runner: Arc<dyn InstallRunner>,
    data_root: PathBuf,
    policy: InstallPhasePolicy,
}

/// The rows and knobs one install run resolved at claim time.
struct Resolved {
    instance: Instance,
    egg: Egg,
    script: Option<EggScript>,
    /// The combined environment the script sees.
    env: Vec<(String, String)>,
}

/// How the pre-script stop ended.
enum Stopped {
    /// The workload was (or already is) offline and quiesced.
    Offline,
    /// Cancellation was observed while waiting.
    Cancelled,
    /// The wait failed or timed out.
    Failed(String),
}

impl InstallJobHandler {
    /// Build the handler; [`crate::InstallService::register`]
    /// installs it on the executor for both kinds.
    pub fn new(
        persistence: Arc<dyn Persistence>,
        supervisor: Arc<Supervisor>,
        runner: Arc<dyn InstallRunner>,
        data_root: PathBuf,
    ) -> Self {
        Self {
            persistence,
            supervisor,
            runner,
            data_root,
            policy: InstallPhasePolicy::default(),
        }
    }

    /// Build the handler with explicit phase timings (tests).
    pub fn with_policy(
        persistence: Arc<dyn Persistence>,
        supervisor: Arc<Supervisor>,
        runner: Arc<dyn InstallRunner>,
        data_root: PathBuf,
        policy: InstallPhasePolicy,
    ) -> Self {
        Self {
            persistence,
            supervisor,
            runner,
            data_root,
            policy,
        }
    }

    // ── Resolution ─────────────────────────────────────────────

    /// Resolve the instance, its egg, the script the job's kind
    /// selects, and the script's environment.
    fn resolve_sync(persistence: &dyn Persistence, job: &Job) -> Option<Resolved> {
        let instance = persistence.get_instance(&job.instance_id).ok().flatten()?;
        let egg = persistence.get_egg(&instance.egg_id).ok().flatten()?;

        // The skip hatch rides the payload; the row's flag is the
        // default the service already folded in (the payload
        // carries the folded value).
        let skip = job
            .payload
            .as_deref()
            .and_then(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())
            .and_then(|value| value.get("skip_egg_scripts").and_then(|s| s.as_bool()))
            .unwrap_or(false);

        let script = if skip {
            None
        } else {
            match job.kind {
                JobKind::Install => egg.scripts.install.clone(),
                // Update: the update script, else the install
                // script (egg-change reinstall parity).
                JobKind::Update => egg
                    .scripts
                    .update
                    .clone()
                    .or_else(|| egg.scripts.install.clone()),
                _ => None,
            }
        };

        let env = Self::script_env(persistence, &instance, &egg);
        Some(Resolved {
            instance,
            egg,
            script,
            env,
        })
    }

    /// The environment the script sees: the instance's runtime env
    /// (`SERVER_MEMORY`/`SERVER_PORT`/`SERVER_IP` + Wings-parity
    /// `HOME`/`USER`) plus the egg's variables at their default
    /// values (per-instance overrides arrive with the instance
    /// variable surface; the defaults are what a fresh install
    /// seeds).
    fn script_env(
        persistence: &dyn Persistence,
        instance: &Instance,
        egg: &Egg,
    ) -> Vec<(String, String)> {
        let mib = instance.limits.memory.0 / (1024 * 1024);
        let mut env = vec![
            ("SERVER_MEMORY".to_owned(), mib.to_string()),
            ("HOME".to_owned(), "/home/container".to_owned()),
            ("USER".to_owned(), "container".to_owned()),
        ];
        // The primary allocation supplies the addressing triple;
        // an instance without one (created allocation-less) keeps
        // the legacy placeholder.
        if let Ok(Some(allocation)) = persistence.find_allocation_by_instance(&instance.id) {
            env.push(("SERVER_PORT".to_owned(), allocation.port.to_string()));
            env.push(("SERVER_IP".to_owned(), allocation.ip.to_string()));
        } else {
            env.push(("SERVER_PORT".to_owned(), "0".to_owned()));
            env.push(("SERVER_IP".to_owned(), "127.0.0.1".to_owned()));
        }
        // Egg variables at their defaults; a duplicate env_name
        // (the egg validator forbids them) would just overwrite.
        for variable in &egg.variables {
            env.push((variable.env_name.clone(), variable.default_value.clone()));
        }
        env
    }

    // ── The pre-script stop ────────────────────────────────────

    /// Whether the workload should be treated as running at the
    /// start of the install (the transfer handler's vocabulary).
    fn was_running(snapshot: &zim_engine_application::supervision::ObservedSnapshot) -> bool {
        match snapshot.observed {
            ObservedState::Running { .. } | ObservedState::Starting => true,
            ObservedState::Stopping { .. } | ObservedState::Unknown => {
                snapshot.desired.wants_running()
            }
            ObservedState::Offline { .. } => false,
        }
    }

    /// Stop the workload if it is running, then wait for `Offline`.
    async fn stop_workload(&self, ctx: &JobContext, instance: InstanceId) -> Stopped {
        let snapshot = match self.supervisor.observed(&instance).await {
            Ok(snapshot) => snapshot,
            Err(e) => return Stopped::Failed(e.to_string()),
        };
        if matches!(snapshot.observed, ObservedState::Unknown) {
            // An unreachable runtime cannot confirm the workload is
            // stopped; refuse to run a script over a
            // maybe-running workload's data directory.
            return Stopped::Failed(
                "the workload's state cannot be observed (runtime unreachable)".to_owned(),
            );
        }
        if !Self::was_running(&snapshot) {
            return Stopped::Offline;
        }
        ctx.progress("stopping", 0.0);
        let stop = PowerAction::Stop {
            deadline: self.policy.stop_deadline,
        };
        if let Err(e) = self.supervisor.request_power(&instance, stop).await {
            return Stopped::Failed(format!("the workload could not be stopped: {e}"));
        }
        let deadline = tokio::time::Instant::now() + self.policy.stop_wait;
        loop {
            if ctx.is_cancelled() {
                return Stopped::Cancelled;
            }
            match self.supervisor.observed(&instance).await {
                Ok(snapshot) => {
                    if let ObservedState::Offline { .. } = snapshot.observed {
                        ctx.progress("stopping", 1.0);
                        return Stopped::Offline;
                    }
                }
                Err(e) => return Stopped::Failed(e.to_string()),
            }
            if tokio::time::Instant::now() >= deadline {
                return Stopped::Failed(
                    "the workload did not stop in time; retry the install".to_owned(),
                );
            }
            tokio::time::sleep(self.policy.poll_interval).await;
        }
    }

    // ── The post-script stamp ──────────────────────────────────

    /// Stamp `installed_at` on the instance row (read-modify-write
    /// in one blocking hop; the supervisor's concurrent writes are
    /// column-scoped `update_instance_status` calls, and after the
    /// pre-script stop the workload's observed state is stable —
    /// the quiesced window this runs in is race-free in practice).
    fn stamp_installed_sync(persistence: &dyn Persistence, instance_id: InstanceId) {
        let Ok(Some(mut instance)) = persistence.get_instance(&instance_id) else {
            return;
        };
        instance.installed_at = Some(Utc::now());
        let _ = persistence.put_instance(&instance);
    }
}

#[async_trait]
impl JobHandler for InstallJobHandler {
    async fn run(&self, ctx: JobContext) -> JobOutcome {
        let job = ctx.job().clone();

        // ── Resolve ────────────────────────────────────────────
        let resolved = tokio::task::spawn_blocking({
            let persistence = Arc::clone(&self.persistence);
            let job = job.clone();
            move || Self::resolve_sync(&*persistence, &job)
        })
        .await
        .expect("install row resolution task panicked");

        let Some(Resolved {
            instance,
            egg: _egg,
            script,
            env,
        }) = resolved
        else {
            // The instance (or its egg) was deleted out from under
            // the queued job: nothing was written, nothing to clean.
            return JobOutcome::Failed {
                message: "the install's instance or egg can no longer be resolved".to_owned(),
                cleanup_pending: false,
            };
        };

        // A skip-scripts install (or an egg with no script at all):
        // the whole job is the stamp.
        let Some(script) = script else {
            let persistence = Arc::clone(&self.persistence);
            let id = instance.id;
            tokio::task::spawn_blocking(move || Self::stamp_installed_sync(&*persistence, id))
                .await
                .expect("install stamp task panicked");
            return JobOutcome::Succeeded;
        };

        // ── Stop the workload ───────────────────────────────────
        match self.stop_workload(&ctx, instance.id).await {
            Stopped::Offline => {}
            Stopped::Cancelled => return JobOutcome::Cancelled,
            Stopped::Failed(message) => {
                return JobOutcome::Failed {
                    message,
                    cleanup_pending: false,
                };
            }
        }
        if ctx.is_cancelled() {
            return JobOutcome::Cancelled;
        }

        // ── Pull + run the script ───────────────────────────────
        let data_dir = instance_data_dir(&self.data_root, &instance.uuid_short);
        let spec = InstallScriptSpec {
            image: script.container.to_string(),
            entrypoint: script.entrypoint.clone(),
            script: script.script.clone(),
            env,
            data_dir: data_dir.clone(),
            container_data_dir: PathBuf::from("/home/container"),
            // A unique, readable container name per run: the
            // instance's short UUID + the job id's leading hex.
            container_name: {
                let hex = job.id.to_string().replace('-', "");
                let suffix: String = hex.chars().take(8).collect();
                format!("{}_installer_{suffix}", instance.uuid_short)
            },
            timeout: self.policy.install_timeout,
            labels: vec![
                ("ZIM.InstanceId".to_owned(), instance.id.to_string()),
                ("ZIM.JobId".to_owned(), job.id.to_string()),
            ],
        };

        // Bridge the executor's progress reporter (job.progress
        // events) and cancellation token onto the port's hooks.
        let progress_fn: InstallProgressFn = match ctx.progress_fn() {
            Some(report) => report,
            None => Arc::new(|_, _| {}),
        };
        let cancel = ctx.cancel().clone();
        let is_cancelled: InstallCancelledFn = Arc::new(move || cancel.is_cancelled());

        let run = self
            .runner
            .run_install_script(&spec, progress_fn, is_cancelled)
            .await;

        match run {
            Ok(InstallRunOutcome::Completed {
                exit_code: Some(0),
                tail: _,
            }) => {
                // Success: stamp installed_at (the workload stays
                // stopped — Pterodactyl reinstall parity).
                let persistence = Arc::clone(&self.persistence);
                let id = instance.id;
                tokio::task::spawn_blocking(move || Self::stamp_installed_sync(&*persistence, id))
                    .await
                    .expect("install stamp task panicked");
                JobOutcome::Succeeded
            }
            Ok(InstallRunOutcome::Completed { exit_code, tail }) => {
                // The script failed: exit code + tail in the
                // message; partial data is retry state (the module
                // docs).
                let mut message = match exit_code {
                    Some(code) => format!("the install script exited with status {code}"),
                    None => "the install script's exit status is unknown".to_owned(),
                };
                let tail = tail.trim();
                if !tail.is_empty() {
                    message.push_str("; output tail:\n");
                    message.push_str(tail);
                }
                JobOutcome::Failed {
                    message,
                    cleanup_pending: false,
                }
            }
            Ok(InstallRunOutcome::Cancelled) => JobOutcome::Cancelled,
            Err(InstallRunError::TimedOut(after)) => JobOutcome::Failed {
                message: format!(
                    "the install script exceeded its {} second deadline and was killed",
                    after.as_secs()
                ),
                cleanup_pending: false,
            },
            Err(InstallRunError::Runtime(e)) => JobOutcome::Failed {
                message: format!("the install container could not run: {e}"),
                cleanup_pending: false,
            },
        }
    }

    async fn cleanup(&self, _job: &Job) -> Result<(), String> {
        // Installs never answer `cleanup_pending = true`: the
        // partial data belongs to the instance (retry state), and
        // the runner removes the installer container on every exit
        // path. Nothing to do is success (the cleaner's contract).
        Ok(())
    }
}
