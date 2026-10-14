//! The install-script runner port: one-shot constrained containers
//! for untrusted egg scripts (M8.3).
//!
//! An install (or update) script is *not* the instance's workload:
//! it is an arbitrary shell script from an imported egg, run once,
//! in a container image the egg names, writing into the instance's
//! data directory. That is a different execution target than the
//! per-instance [`Runtime`](super::Runtime) handle (which addresses
//! the workload's own container, recreated from the persisted
//! row), so it gets its own port instead of a method bolted onto
//! the runtime trait.
//!
//! The contract the engine relies on (roadmap §M8, RUST_ENGINE.md
//! §8 "Install job"):
//!
//! - the script runs **constrained**: capabilities dropped, no new
//!   privileges, read-only root filesystem, an unprivileged user,
//!   no published ports, and only the instance data directory
//!   writable (plus a bounded `/tmp`). Untrusted egg install
//!   scripts never run with engine privileges.
//! - the runner is **bounded**: a deadline kills and removes the
//!   container when the script exceeds it.
//! - the runner is **cancellable**: a cooperative hook is polled
//!   while the script runs; on cancellation the container is
//!   killed and removed and the run answers
//!   [`InstallRunOutcome::Cancelled`].
//! - the runner **reports progress** through the same
//!   `(stage, fraction)` vocabulary the job executor's
//!   `job.progress` events use (`pulling_image`,
//!   `running_install_script`).
//!
//! The Docker adapter (`zim_engine_runtime_docker`) implements the
//! port; [`NullInstallRunner`] answers `Unsupported` for the
//! degraded API-only engine (no reachable daemon); tests bind
//! `MockInstallRunner` (feature `test-util`).

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use super::state::RuntimeError;

/// Progress reporter handed to a runner: `(stage, fraction)` with
/// the stage a stable snake_case name and the fraction 0.0–1.0
/// (out-of-range values are the caller's documentation problem;
/// runners pass through what they observe).
pub type InstallProgressFn = Arc<dyn Fn(&str, f32) + Send + Sync>;

/// Cooperative cancellation hook handed to a runner: answers
/// whether the driving job was cancelled. Runners poll it while
/// the script runs (between daemon round-trips is frequent
/// enough).
pub type InstallCancelledFn = Arc<dyn Fn() -> bool + Send + Sync>;

/// Everything a runner needs to execute one egg script.
#[derive(Debug, Clone)]
pub struct InstallScriptSpec {
    /// The container image the egg names for the script (e.g.
    /// `ghcr.io/parkervcp/installers:alpine`).
    pub image: String,
    /// The entrypoint the egg names (`bash` in virtually all
    /// legacy eggs); empty means the image's default.
    pub entrypoint: String,
    /// The script body, verbatim.
    pub script: String,
    /// The environment the script sees: the instance's runtime env
    /// (SERVER_MEMORY/SERVER_PORT/SERVER_IP + HOME/USER) plus the
    /// egg's variables.
    pub env: Vec<(String, String)>,
    /// The instance's host-side data directory (bind source; the
    /// runner creates it when missing so the bind is never
    /// root-created).
    pub data_dir: PathBuf,
    /// The in-container mount point for the data directory
    /// (`/home/container`, the legacy convention).
    pub container_data_dir: PathBuf,
    /// A unique container name for this run (the caller derives it
    /// from the instance + job ids).
    pub container_name: String,
    /// How long the script may run before it is killed.
    pub timeout: std::time::Duration,
    /// Extra labels for the container (beyond the runner's own
    /// `Service=ZIM` / `ContainerType=server_install`).
    pub labels: Vec<(String, String)>,
}

/// How a completed run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallRunOutcome {
    /// The container ran to completion. `exit_code` is the
    /// script's exit status (`None` when the daemon did not report
    /// one); `tail` is the trailing output (stdout + stderr, best
    /// effort, bounded) for diagnostics and failure messages.
    Completed {
        /// The script's exit status.
        exit_code: Option<i32>,
        /// Trailing combined output, bounded (the failure message
        /// carries it; success paths ignore it).
        tail: String,
    },
    /// Cancellation was observed mid-run; the container was killed
    /// and removed. Partial downloads stay in the data directory
    /// (a retry install re-runs the script over them).
    Cancelled,
}

/// Why a run could not produce an outcome.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstallRunError {
    /// The backend failed or was unreachable.
    #[error("{0}")]
    Runtime(#[from] RuntimeError),
    /// The script exceeded its deadline; the container was killed
    /// and removed.
    #[error("the install script exceeded its {} second deadline", .0.as_secs())]
    TimedOut(std::time::Duration),
}

/// The install-script runner port (see the module docs). One
/// implementation per runtime backend; the composition root binds
/// the Docker adapter, the degraded engine binds
/// [`NullInstallRunner`], tests bind the mock.
#[async_trait]
pub trait InstallRunner: Send + Sync {
    /// Run one egg script to completion (or cancellation / timeout
    /// / backend failure), reporting progress through `progress`.
    async fn run_install_script(
        &self,
        spec: &InstallScriptSpec,
        progress: InstallProgressFn,
        is_cancelled: InstallCancelledFn,
    ) -> Result<InstallRunOutcome, InstallRunError>;
}

/// The no-backend runner: every script answers `Unsupported`, so
/// install jobs fail fast with a structured message instead of
/// queueing work a degraded engine cannot execute.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullInstallRunner;

#[async_trait]
impl InstallRunner for NullInstallRunner {
    async fn run_install_script(
        &self,
        _spec: &InstallScriptSpec,
        _progress: InstallProgressFn,
        _is_cancelled: InstallCancelledFn,
    ) -> Result<InstallRunOutcome, InstallRunError> {
        Err(RuntimeError::Unsupported {
            feature: "egg install scripts".to_owned(),
            milestone: "no container runtime is reachable (degraded API-only engine)".to_owned(),
        }
        .into())
    }
}

#[cfg(test)]
mod tests {
    //! The port itself carries no behavior beyond the null
    //! adapter's refusal; conformance is verified by the Docker
    //! adapter's integration suite and the installs crate's mock
    //! suite. This keeps a compile-time witness that the trait is
    //! object-safe (`Arc<dyn InstallRunner>`).

    use super::*;

    fn assert_object_safe(_: Option<Arc<dyn InstallRunner>>) {}

    #[tokio::test]
    async fn null_runner_refuses_with_unsupported() {
        let runner = NullInstallRunner;
        let progress: InstallProgressFn = Arc::new(|_, _| {});
        let cancelled: InstallCancelledFn = Arc::new(|| false);
        let spec = InstallScriptSpec {
            image: "ghcr.io/parkervcp/installers:alpine".to_owned(),
            entrypoint: "bash".to_owned(),
            script: "echo hi".to_owned(),
            env: vec![],
            data_dir: PathBuf::from("/tmp/nowhere"),
            container_data_dir: PathBuf::from("/home/container"),
            container_name: "test_installer".to_owned(),
            timeout: std::time::Duration::from_secs(1),
            labels: vec![],
        };
        let err = runner
            .run_install_script(&spec, progress, cancelled)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            InstallRunError::Runtime(RuntimeError::Unsupported { .. })
        ));
        let _ = assert_object_safe;
    }
}
