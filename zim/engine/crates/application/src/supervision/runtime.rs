//! The runtime abstraction: the [`Runtime`] trait per-instance
//! runtimes implement, and the [`RuntimeFactory`] port that creates
//! them.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use super::state::{
    ExecCommand, ExecOutput, InstanceConfig, ObservedState, ResourceUsage, RuntimeError,
    RuntimeKind,
};

/// The runtime abstraction (RUST_ENGINE.md §4).
///
/// One handle addresses one instance's execution target (for the
/// Docker runtime: the instance's container, addressed by the legacy
/// UUID name). The trait is async — every operation is a round-trip
/// to a runtime backend — and object-safe through `#[async_trait]`
/// so the supervision module can hold `Arc<dyn Runtime>` (see the
/// module docs for the recorded deviation from the §4 sketch's
/// synchronous form).
///
/// # Contract notes for implementors
///
/// - `state()`/`exit_state()` are observations, not commands: they
///   must be safe to call concurrently with any other operation and
///   must not mutate the runtime's target.
/// - `start()` recreates the target from `cfg` (Wings's
///   `OnBeforeStart` behavior): an existing container is removed and
///   re-created so the persisted row always wins.
/// - `stop(deadline)` is graceful-then-forcible: request stop, wait
///   up to `deadline`, then kill if the target is still running.
/// - Not-found results on stop/kill are `Ok(())` (already in the
///   requested state), matching Wings's 404 tolerance.
/// - `send_input`/console attach require a persistent attachment
///   stream; the M6 surface returns `Unsupported` from runtimes that
///   do not maintain one (the Docker adapter ships it with M7).
#[async_trait]
pub trait Runtime: Send + Sync {
    /// Which class of runtime this is (invariant 8).
    fn kind(&self) -> RuntimeKind;

    /// The currently observed state of this instance's target.
    ///
    /// A missing target reports `Offline { exit_code: None,
    /// oom_killed: false }`; an unreachable backend reports
    /// `Err(Unavailable)` (the supervision layer turns that into
    /// observed `Unknown`).
    async fn state(&self) -> Result<ObservedState, RuntimeError>;

    /// (Re)create and start the instance's target from `cfg`.
    async fn start(&self, cfg: &InstanceConfig) -> Result<(), RuntimeError>;

    /// Graceful stop bounded by `deadline` (SIGTERM-or-equivalent
    /// first, forcible kill after the deadline).
    async fn stop(&self, deadline: Duration) -> Result<(), RuntimeError>;

    /// Immediate forcible kill.
    async fn kill(&self) -> Result<(), RuntimeError>;

    /// Run a one-shot command inside the instance's target.
    async fn exec(&self, cmd: &ExecCommand) -> Result<ExecOutput, RuntimeError>;

    /// Write to the target's stdin (console input; M7 surface).
    async fn send_input(&self, input: &[u8]) -> Result<(), RuntimeError>;

    /// The last-known exit state: `(exit_code, oom_killed)`.
    ///
    /// `(None, false)` when no exit has been observed (running, or
    /// never started).
    async fn exit_state(&self) -> Result<(Option<i32>, bool), RuntimeError>;

    /// A one-shot resource-usage sample.
    async fn stats(&self) -> Result<ResourceUsage, RuntimeError>;
}

/// Creates per-instance [`Runtime`] handles (a port: the composition
/// root binds the Docker factory; tests bind mocks).
#[async_trait]
pub trait RuntimeFactory: Send + Sync {
    /// Create the runtime handle for one instance configuration.
    ///
    /// Called when the supervisor loads an instance; the returned
    /// handle addresses the instance's target for its whole
    /// supervised lifetime (the supervision layer refreshes
    /// [`InstanceConfig`] separately).
    async fn create(&self, cfg: &InstanceConfig) -> Result<Arc<dyn Runtime>, RuntimeError>;

    /// Probe the backend's liveness (e.g. a Docker daemon ping).
    ///
    /// Used at enqueue time so power operations fail fast with a
    /// structured `runtime_unavailable` instead of being accepted
    /// into a queue that cannot act on them.
    async fn check(&self) -> Result<(), RuntimeError>;
}

#[cfg(test)]
mod tests {
    //! The traits themselves carry no behavior; conformance is
    //! verified by the mock-runtime supervision tests and the Docker
    //! adapter's integration suite. This keeps a compile-time witness
    //! that the traits are object-safe (the supervision module's
    //! `Arc<dyn Runtime>` / `Arc<dyn RuntimeFactory>` requirement).

    use super::*;

    #[test]
    fn runtime_traits_are_object_safe() {
        fn assert_runtime_send_sync(_r: &(dyn Runtime + Send + Sync)) {}
        fn assert_factory_send_sync(_f: &(dyn RuntimeFactory + Send + Sync)) {}
        // Object safety is proven by being able to name the trait
        // objects at all:
        let _runtime_type: Option<std::sync::Arc<dyn Runtime>> = None;
        let _factory_type: Option<std::sync::Arc<dyn RuntimeFactory>> = None;
        let _ = assert_runtime_send_sync;
        let _ = assert_factory_send_sync;
    }
}
