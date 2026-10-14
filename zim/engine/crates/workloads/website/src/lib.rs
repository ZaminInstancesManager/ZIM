//! The Website workload integration (Website, M16 skeleton).
//!
//! Declared with `#[workload_integration]`; the behavioral surface
//! (spec schemas, lifecycle semantics) arrives with the kind's
//! implementation milestone.
//!
//! Understands (the M19 scope): web server configuration, health
//! checks, deploy strategies (the §10.15 deploy mutation), HTTPS,
//! and domains.
//!
//! ```text
//! id      = org.zaminmc.workload.website
//! kind    = Website
//! requires: HealthCheckCapable, FilesystemCapable, NetworkCapable
//! optional: (none)
//! runtimes: Container, Remote
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use zim_engine_workloads_sdk::workload_integration;

/// The Website integration's singleton.
#[workload_integration(
    kind = Website,
    id = "org.zaminmc.workload.website",
    version = "0.1.0",
    display_name = "Website",
    description = "Static and dynamic web applications.",
    required_capabilities = [HealthCheckCapable, FilesystemCapable, NetworkCapable],
    optional_capabilities = [],
    runtime_kinds = [Container, Remote],
)]
pub struct WebsiteIntegration;

#[cfg(test)]
mod tests {
    use super::*;
    use zim_engine_domain::WorkloadKind;
    use zim_engine_workloads_sdk::{Capability, RuntimeKindRef, WorkloadIntegration};

    #[test]
    fn declares_the_website_contract() {
        assert_eq!(WebsiteIntegration.kind(), WorkloadKind::Website);
        let manifest = WebsiteIntegration.manifest();
        assert_eq!(manifest.id, "org.zaminmc.workload.website");
        assert_eq!(manifest.display_name, "Website");
        assert!(manifest.validate().is_ok());
        assert!(manifest.accepts_runtime(RuntimeKindRef::Container));
        assert!(manifest
            .required_capabilities
            .contains(Capability::HealthCheckCapable));
        assert!(manifest
            .required_capabilities
            .contains(Capability::FilesystemCapable));
        assert!(manifest
            .required_capabilities
            .contains(Capability::NetworkCapable));
        // The behavioral hooks are still the SDK's defaults.
        assert!(WebsiteIntegration.runtime_requirements() == manifest.required_capabilities);
    }
}
