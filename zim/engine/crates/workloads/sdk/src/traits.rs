//! The `WorkloadIntegration` trait and the §4 capability traits.
//!
//! An integration implements [`WorkloadIntegration`] to register a
//! kind: its manifest (identity + requirements), its spec schema,
//! and — as the real integrations land (M17+) — its lifecycle
//! semantics. The trait's behavioral hooks carry default
//! implementations that answer `Unsupported`, so an integration
//! grows into its contract one operation at a time.
//!
//! The §4 capability traits (the *runtime*-side contract an
//! integration's requirements are matched against at placement)
//! are declared here per `docs/architecture/RUST_ENGINE.md` §12.4;
//! the runtime adapters' implementations of them arrive with the
//! integration milestones that need them, and the support types are
//! the minimal shapes the doc sketches.

use std::time::Duration;

use serde_json::Value;

use crate::capabilities::{Capability, CapabilitySet};
use crate::errors::{IntegrationError, SpecError};
use crate::manifest::IntegrationManifest;

// ── The integration contract ─────────────────────────────────────

/// What a workload integration is to the engine (§5, §12.4).
///
/// Metadata methods (`manifest`, `kind`, `id`,
/// `runtime_requirements`, `accepts_runtime`) have default
/// implementations that read the manifest — an integration
/// implements only [`manifest`][Self::manifest] plus whatever
/// behavioral hooks it is ready to own.
pub trait WorkloadIntegration: Send + Sync {
    /// The integration's validated declaration.
    fn manifest(&self) -> &'static IntegrationManifest;

    /// The workload kind this integration implements.
    fn kind(&self) -> zim_engine_domain::WorkloadKind {
        self.manifest().kind
    }

    /// The integration's id (its manifest's reverse-DNS identity).
    fn id(&self) -> &str {
        &self.manifest().id
    }

    /// The capabilities a placement must provide for this kind.
    fn runtime_requirements(&self) -> CapabilitySet {
        self.manifest().required_capabilities
    }

    /// Whether a runtime class is acceptable for this kind.
    fn accepts_runtime(&self, kind: crate::manifest::RuntimeKindRef) -> bool {
        self.manifest().accepts_runtime(kind)
    }

    // ── Behavioral hooks (defaulted: M16 ships skeletons) ────────

    /// Validate a workload row's spec against this kind's schema.
    fn validate_spec(&self, _spec: &Value) -> Result<(), SpecError> {
        Ok(())
    }

    /// The kind's default spec (the form the API offers when a
    /// workload is created without one).
    fn default_spec(&self) -> Value {
        Value::Null
    }

    /// Prepare a first-run install for a workload of this kind
    /// (e.g. Minecraft's EULA + JAR bootstrap).
    fn prepare_install(&self, _spec: &Value) -> Result<Value, IntegrationError> {
        Err(IntegrationError::unsupported(self.id(), "prepare_install"))
    }

    /// Translate a lifecycle intention for this kind (e.g. a
    /// database's stop-with-checkpoint vs. a game server's plain
    /// stop).
    fn lifecycle_plan(&self, _action: &str) -> Result<Value, IntegrationError> {
        Err(IntegrationError::unsupported(self.id(), "lifecycle_plan"))
    }
}

impl<T: WorkloadIntegration + ?Sized> WorkloadIntegration for &T {
    fn manifest(&self) -> &'static IntegrationManifest {
        (**self).manifest()
    }
}

// ── The §4 capability traits and their support types ─────────────

/// Readiness, distinct from process presence (§4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// The workload answers its service contract.
    Ready,
    /// The process is up but the service is not ready yet.
    NotReady {
        /// Why it is not ready yet.
        reason: String,
    },
    /// The probe could not determine readiness.
    Unknown,
}

/// Health, distinct from process liveness (§4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    /// Healthy.
    Healthy,
    /// Unhealthy, with the reason.
    Unhealthy {
        /// Why.
        reason: String,
    },
    /// Serving but degraded (a replica lagging, a cache cold).
    Degraded {
        /// Why.
        reason: String,
    },
}

/// One exposed network endpoint of a multi-endpoint workload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// The endpoint's logical name (e.g. `"postgres"`, `"https"`).
    pub name: String,
    /// The port as placed.
    pub port: u16,
}

/// A port the integration asks the runtime to expose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortSpec {
    /// The logical name for the exposed endpoint.
    pub name: String,
    /// The container-side port.
    pub container_port: u16,
    /// The protocol (`"tcp"` or `"udp"`).
    pub protocol: &'static str,
}

/// A reference to a runtime snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRef {
    /// The snapshot's label.
    pub label: String,
}

/// A migration plan (§4 `Migratable`).
#[derive(Debug, Clone)]
pub struct MigrationPlan {
    /// Whether the workload must be quiesced before transfer.
    pub requires_quiesce: bool,
    /// The estimated downtime window.
    pub estimated_downtime: Duration,
}

/// A migration checkpoint (§4 `Migratable`).
#[derive(Debug, Clone)]
pub struct MigrationCheckpoint {
    /// The point-in-time marker to resume or abort from.
    pub marker: String,
}

/// One exec-style command (§4 `ExecCapable`).
#[derive(Debug, Clone)]
pub struct ExecCommand {
    /// The command line.
    pub command: String,
    /// A timeout for the execution.
    pub timeout: Duration,
}

/// The result of one exec-style command.
#[derive(Debug, Clone)]
pub struct ExecOutput {
    /// The exit status, when the command completed.
    pub exit_code: Option<i32>,
    /// The captured stdout.
    pub stdout: String,
    /// The captured stderr.
    pub stderr: String,
}

/// A point-in-time resource-usage sample (§4 `MetricsCapable`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ResourceUsage {
    /// CPU use, normalized to one core (1.0 = one full core).
    pub cpu: f32,
    /// Resident memory, bytes.
    pub memory_bytes: u64,
    /// Bytes of network I/O since the workload started.
    pub network_bytes: u64,
}

/// The runtime accepts text input on a console stream (§4).
pub trait ConsoleCapable: Send + Sync {
    /// The currently buffered console output (a tail window).
    fn console_tail(&self, lines: usize) -> Vec<String>;

    /// Send one line of console input to the workload.
    fn send_input(&self, line: &str) -> Result<(), IntegrationError>;
}

/// Readiness distinct from process presence (§4).
pub trait ReadyProbeCapable: Send + Sync {
    /// The workload's current readiness.
    fn readiness(&self) -> Result<Readiness, IntegrationError>;

    /// Block until ready or the deadline passes.
    fn wait_until_ready(&self, deadline: Duration) -> Result<(), IntegrationError>;
}

/// A health endpoint distinct from process presence (§4).
pub trait HealthCheckCapable: Send + Sync {
    /// The workload's current health.
    fn health(&self) -> Result<Health, IntegrationError>;
}

/// The workload's filesystem (§4).
pub trait FilesystemCapable: Send + Sync {
    /// List one directory's entries.
    fn list_dir(&self, path: &str) -> Result<Vec<String>, IntegrationError>;

    /// Read one file's bytes.
    fn read_file(&self, path: &str) -> Result<Vec<u8>, IntegrationError>;

    /// Write one file's bytes.
    fn write_file(&self, path: &str, content: &[u8]) -> Result<(), IntegrationError>;

    /// Delete one file or directory.
    fn delete(&self, path: &str) -> Result<(), IntegrationError>;
}

/// Multiple network endpoints (§4).
pub trait NetworkCapable: Send + Sync {
    /// The workload's exposed endpoints.
    fn endpoints(&self) -> Result<Vec<Endpoint>, IntegrationError>;

    /// Expose one more port.
    fn expose_port(&self, spec: PortSpec) -> Result<Endpoint, IntegrationError>;

    /// Unexpose an endpoint.
    fn unexpose_port(&self, endpoint: &Endpoint) -> Result<(), IntegrationError>;
}

/// Snapshot-style backups (§4).
pub trait SnapshotCapable: Send + Sync {
    /// Take a snapshot with a label.
    fn snapshot(&self, label: &str) -> Result<SnapshotRef, IntegrationError>;

    /// Restore a snapshot.
    fn restore_snapshot(&self, snapshot: &SnapshotRef) -> Result<(), IntegrationError>;
}

/// Migration with quiesce semantics (§4).
pub trait Migratable: Send + Sync {
    /// The migration plan for the current state.
    fn migration_plan(&self) -> Result<MigrationPlan, IntegrationError>;

    /// Quiesce and produce a checkpoint.
    fn prepare_for_migration(&self) -> Result<MigrationCheckpoint, IntegrationError>;

    /// Resume after the transfer completed.
    fn finalize_migration(&self) -> Result<(), IntegrationError>;

    /// Roll back to the checkpoint.
    fn abort_migration(&self, checkpoint: MigrationCheckpoint) -> Result<(), IntegrationError>;
}

/// One-shot command execution (§4).
pub trait ExecCapable: Send + Sync {
    /// Execute one command in the workload's context.
    fn exec(&self, command: ExecCommand) -> Result<ExecOutput, IntegrationError>;
}

/// Resource-usage metrics (§4).
pub trait MetricsCapable: Send + Sync {
    /// The current usage sample.
    fn metrics(&self) -> Result<ResourceUsage, IntegrationError>;
}

/// Every capability trait's name ↔ the [`Capability`] vocabulary.
pub fn capability_of_trait_name(name: &str) -> Option<Capability> {
    Capability::parse(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{IntegrationManifest, RuntimeKindRef};

    struct ExampleIntegration;

    impl WorkloadIntegration for ExampleIntegration {
        fn manifest(&self) -> &'static IntegrationManifest {
            static MANIFEST: std::sync::LazyLock<IntegrationManifest> =
                std::sync::LazyLock::new(|| {
                    IntegrationManifest::builtin(
                        zim_engine_domain::WorkloadKind::GameServer,
                        "org.example.workload.example",
                        "0.1.0",
                        "Example",
                        "The trait-contract test double.",
                        [Capability::ConsoleCapable, Capability::MetricsCapable]
                            .into_iter()
                            .collect(),
                        CapabilitySet::EMPTY,
                        &[RuntimeKindRef::Container],
                    )
                });
            &MANIFEST
        }
    }

    #[test]
    fn default_methods_read_the_manifest() {
        let integration = ExampleIntegration;
        assert_eq!(
            integration.kind(),
            zim_engine_domain::WorkloadKind::GameServer
        );
        assert_eq!(integration.id(), "org.example.workload.example");
        assert_eq!(integration.runtime_requirements().len(), 2);
        assert!(integration.accepts_runtime(RuntimeKindRef::Container));
        assert!(!integration.accepts_runtime(RuntimeKindRef::Remote));
    }

    #[test]
    fn behavioral_hooks_default_to_unsupported() {
        let integration = ExampleIntegration;
        assert!(matches!(
            integration.prepare_install(&Value::Null),
            Err(IntegrationError::Unsupported { .. })
        ));
        assert!(matches!(
            integration.lifecycle_plan("stop"),
            Err(IntegrationError::Unsupported { .. })
        ));
        // Spec hooks default permissive.
        assert_eq!(integration.validate_spec(&Value::Null), Ok(()));
        assert_eq!(integration.default_spec(), Value::Null);
    }

    #[test]
    fn references_forward_the_trait() {
        let reference: &ExampleIntegration = &ExampleIntegration;
        assert_eq!(
            WorkloadIntegration::id(reference),
            "org.example.workload.example"
        );
    }

    #[test]
    fn trait_names_map_to_capabilities() {
        for capability in Capability::ALL {
            assert_eq!(
                capability_of_trait_name(capability.as_str()),
                Some(capability)
            );
        }
        assert_eq!(capability_of_trait_name("Nope"), None);
    }
}
