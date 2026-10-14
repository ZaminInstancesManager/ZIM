//! The Database workload integration (Database, M16 skeleton).
//!
//! Declared with `#[workload_integration]`; the behavioral surface
//! (spec schemas, lifecycle semantics) arrives with the kind's
//! implementation milestone.
//!
//! Understands (the M18 scope): initialization, readiness probes
//! (distinct from process presence), credentials, backup consistency
//! (logical vs. physical), and connection endpoints.
//!
//! ```text
//! id      = org.zaminmc.workload.database
//! kind    = Database
//! requires: ReadyProbeCapable, FilesystemCapable, SnapshotCapable, NetworkCapable, MetricsCapable
//! optional: Migratable
//! runtimes: Container, Remote
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use zim_engine_workloads_sdk::workload_integration;

/// The Database integration's singleton.
#[workload_integration(
    kind = Database,
    id = "org.zaminmc.workload.database",
    version = "0.1.0",
    display_name = "Database",
    description = "Managed databases: Postgres, MySQL/MariaDB, Redis, MongoDB.",
    required_capabilities = [ReadyProbeCapable, FilesystemCapable, SnapshotCapable, NetworkCapable, MetricsCapable],
    optional_capabilities = [Migratable],
    runtime_kinds = [Container, Remote],
)]
pub struct DatabaseIntegration;

#[cfg(test)]
mod tests {
    use super::*;
    use zim_engine_domain::WorkloadKind;
    use zim_engine_workloads_sdk::{Capability, RuntimeKindRef, WorkloadIntegration};

    #[test]
    fn declares_the_database_contract() {
        assert_eq!(DatabaseIntegration.kind(), WorkloadKind::Database);
        let manifest = DatabaseIntegration.manifest();
        assert_eq!(manifest.id, "org.zaminmc.workload.database");
        assert_eq!(manifest.display_name, "Database");
        assert!(manifest.validate().is_ok());
        assert!(manifest.accepts_runtime(RuntimeKindRef::Container));
        assert!(manifest
            .required_capabilities
            .contains(Capability::ReadyProbeCapable));
        assert!(manifest
            .required_capabilities
            .contains(Capability::FilesystemCapable));
        assert!(manifest
            .required_capabilities
            .contains(Capability::SnapshotCapable));
        assert!(manifest
            .required_capabilities
            .contains(Capability::NetworkCapable));
        assert!(manifest
            .required_capabilities
            .contains(Capability::MetricsCapable));
        assert!(manifest
            .optional_capabilities
            .contains(Capability::Migratable));
        // The behavioral hooks are still the SDK's defaults.
        assert!(DatabaseIntegration.runtime_requirements() == manifest.required_capabilities);
    }
}
