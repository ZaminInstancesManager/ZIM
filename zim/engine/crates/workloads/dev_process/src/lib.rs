//! The Dev Process workload integration (DevProcess, M16 skeleton).
//!
//! Declared with `#[workload_integration]`; the behavioral surface
//! (spec schemas, lifecycle semantics) arrives with the kind's
//! implementation milestone.
//!
//! Understands (the M19 scope): file-watch restart, log streams,
//! and the absence of install/backup expectations (a dev loop is
//! ephemeral by design).
//!
//! ```text
//! id      = org.zaminmc.workload.dev_process
//! kind    = DevProcess
//! requires: ConsoleCapable
//! optional: FilesystemCapable, MetricsCapable
//! runtimes: Container, Host
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use zim_engine_workloads_sdk::workload_integration;

/// The Dev Process integration's singleton.
#[workload_integration(
    kind = DevProcess,
    id = "org.zaminmc.workload.dev_process",
    version = "0.1.0",
    display_name = "Dev Process",
    description = "Long-running development processes (cargo watch, npm run dev, ...).",
    required_capabilities = [ConsoleCapable],
    optional_capabilities = [FilesystemCapable, MetricsCapable],
    runtime_kinds = [Container, Host],
)]
pub struct DevProcessIntegration;

#[cfg(test)]
mod tests {
    use super::*;
    use zim_engine_domain::WorkloadKind;
    use zim_engine_workloads_sdk::{Capability, RuntimeKindRef, WorkloadIntegration};

    #[test]
    fn declares_the_dev_process_contract() {
        assert_eq!(DevProcessIntegration.kind(), WorkloadKind::DevProcess);
        let manifest = DevProcessIntegration.manifest();
        assert_eq!(manifest.id, "org.zaminmc.workload.dev_process");
        assert_eq!(manifest.display_name, "Dev Process");
        assert!(manifest.validate().is_ok());
        assert!(manifest.accepts_runtime(RuntimeKindRef::Container));
        assert!(manifest
            .required_capabilities
            .contains(Capability::ConsoleCapable));
        assert!(manifest
            .optional_capabilities
            .contains(Capability::FilesystemCapable));
        assert!(manifest
            .optional_capabilities
            .contains(Capability::MetricsCapable));
        // The behavioral hooks are still the SDK's defaults.
        assert!(DevProcessIntegration.runtime_requirements() == manifest.required_capabilities);
    }
}
