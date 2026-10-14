//! Supervision: per-instance lifecycle, power-action serialization,
//! crash detection, and the runtime abstraction.
//!
//! # Source of truth
//!
//! This module is the M6 realization of the supervision module
//! sketched in `docs/architecture/RUST_ENGINE.md` §4 (the engine's
//! analogue of Wings's `server/` package). It owns:
//!
//! - the [`Runtime`] trait — the runtime abstraction the Docker
//!   adapter (`zim_engine_runtime_docker`) implements;
//! - [`SupervisedInstance`] — desired state, observed state, the
//!   single-worker power queue (Wings invariant 9), the crash state,
//!   and the generation counter (invariant 13);
//! - [`Supervisor`] — the registry that loads instances from
//!   persistence, reconciles persisted vs. observed state on engine
//!   restart (invariants 1 and 10), and fronts the API's power
//!   operations.
//!
//! # Deviations from the §4 sketch, each documented at its site
//!
//! - The sketch's `Runtime` methods are synchronous; the real trait
//!   is `async` (a Docker round-trip is inherently async) and uses
//!   `#[async_trait]` so `Arc<dyn Runtime>` stays object-safe.
//! - The sketch stores `Box<dyn Runtime>` per instance; the module
//!   stores `Arc<dyn Runtime>` because the power worker and the
//!   API's state queries share one runtime handle.
//! - The sketch's `Runtime::generation()` counter lives here on the
//!   supervision side (the runtime reports state; supervision stamps
//!   generations), and the sketch's `subscribers: StreamingSet`
//!   field is realized as the shared `StreamHub` (M7): each
//!   supervised instance publishes lifecycle
//!   (`instance.state_change`) and resource-usage
//!   (`instance.resource_usage`) events onto it.
//! - Crash observation in M6 is a per-instance polling monitor; the
//!   event-driven form (runtime events carrying generations) arrives
//!   with M7 streaming. Stale-sample protection (invariant 13) is
//!   enforced by comparing the generation around each poll.
//!
//! # Persistence
//!
//! The supervisor persists *observed* status transitions through the
//! [`Persistence`](crate::ports::Persistence) port (bridged with
//! `spawn_blocking`, per the M3 decision) so the engine's list views
//! reflect live state — while never treating the persisted value as
//! proof of liveness (invariant 1: every boot re-reconciles from the
//! runtime's actual state).

mod crash;
mod install;
mod instance;
mod null;
mod runtime;
mod state;
mod supervisor;

pub use crash::{CrashLoopPolicy, CrashState};
pub use install::{
    InstallCancelledFn, InstallProgressFn, InstallRunError, InstallRunOutcome, InstallRunner,
    InstallScriptSpec, NullInstallRunner,
};
pub use instance::{
    build_instance_config, instance_data_dir, ObservedSnapshot, PowerAction, PowerError,
    SupervisedInstance, SupervisionPolicy,
};
pub use null::NullRuntimeFactory;
pub use runtime::{Runtime, RuntimeFactory};
pub use state::{
    ConsoleAttachment, DesiredState, ExecCommand, ExecOutput, InstanceConfig, ObservedState,
    PortBinding, PortProtocol, ResourceUsage, RuntimeError, RuntimeKind,
};
pub use supervisor::{ReconcileReport, Supervisor};

#[cfg(feature = "test-util")]
pub mod testing;
