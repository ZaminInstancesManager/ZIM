//! The Generic Container workload integration (GenericContainer, M16 skeleton).
//!
//! Declared with `#[workload_integration]`; the behavioral surface
//! (spec schemas, lifecycle semantics) arrives with the kind's
//! implementation milestone.
//!
//! The one kind with no fixed requirements: the workload's spec
//! declares the capabilities its placement negotiates (§5), and the
//! integration's manifest lists the full vocabulary as optional.
//!
//! ```text
//! id      = org.zaminmc.workload.generic_container
//! kind    = GenericContainer
//! requires: (negotiated at placement)
//! optional: ConsoleCapable, ReadyProbeCapable, HealthCheckCapable, FilesystemCapable, NetworkCapable, SnapshotCapable, Migratable, ExecCapable, MetricsCapable
//! runtimes: Container, Remote
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use zim_engine_workloads_sdk::workload_integration;

/// The Generic Container integration's singleton.
#[workload_integration(
    kind = GenericContainer,
    id = "org.zaminmc.workload.generic_container",
    version = "0.1.0",
    display_name = "Generic Container",
    description = "Arbitrary containerized workloads with placement-time capability negotiation.",
    required_capabilities = [],
    optional_capabilities = [ConsoleCapable, ReadyProbeCapable, HealthCheckCapable, FilesystemCapable, NetworkCapable, SnapshotCapable, Migratable, ExecCapable, MetricsCapable],
    runtime_kinds = [Container, Remote],
)]
pub struct GenericContainerIntegration;

#[cfg(test)]
mod tests {
    use super::*;
    use zim_engine_domain::WorkloadKind;
    use zim_engine_workloads_sdk::{Capability, RuntimeKindRef, WorkloadIntegration};

    #[test]
    fn declares_the_generic_container_contract() {
        assert_eq!(
            GenericContainerIntegration.kind(),
            WorkloadKind::GenericContainer
        );
        let manifest = GenericContainerIntegration.manifest();
        assert_eq!(manifest.id, "org.zaminmc.workload.generic_container");
        assert_eq!(manifest.display_name, "Generic Container");
        assert!(manifest.validate().is_ok());
        assert!(manifest.accepts_runtime(RuntimeKindRef::Container));
        assert!(manifest
            .optional_capabilities
            .contains(Capability::ConsoleCapable));
        assert!(manifest
            .optional_capabilities
            .contains(Capability::ReadyProbeCapable));
        // The behavioral hooks are still the SDK's defaults.
        assert!(
            GenericContainerIntegration.runtime_requirements() == manifest.required_capabilities
        );
    }
}
