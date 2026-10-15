//! The GenericContainer workload integration (M19).
//!
//! Declared per the §5 contract (identity, kind, capabilities,
//! runtime classes) in the hand-written form — the SDK example's
//! "Form 2" (the same posture the Database (M18), Website, and
//! DevProcess kinds take in M19).
//!
//! The kind's own headline is the placement contract: **the
//! manifest declares no required capabilities** — the
//! requirements live in each workload's spec ("whatever the
//! spec declares is what the runtime must provide"), and the
//! placement-time negotiation ([`crate::negotiate`]) is the §4
//! enforcement model's surface.
//!
//! The M19 behavioral surface:
//!
//! - [`GenericContainerSpec`] parsing (image, command, env,
//!   endpoints, health check, resource limits, required
//!   capabilities);
//! - [`negotiate`]/[`negotiate_or_reject`] — the placement-time
//!   capability negotiation;
//! - [`lifecycle_plan`](GenericContainerIntegration::lifecycle_plan) —
//!   the transparent power translations (the container is the
//!   user's own; the image's PID 1 is the convention);
//! - [`GenericContainerWorkload`] — the per-workload §4
//!   capability handle (health check, filesystem, network, exec,
//!   metrics) over a live runtime.
//!
//! ```text
//! id      = org.zaminmc.workload.generic_container
//! kind    = GenericContainer
//! requires: (none — the spec's own declarations are the requirements)
//! optional: every capability in the vocabulary
//! runtimes: Container, Remote
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod handle;
pub mod lifecycle;
pub mod negotiate;
pub mod spec;

use serde_json::Value;
use std::sync::LazyLock;

use zim_engine_workloads_sdk::errors::{IntegrationError, SpecError};
use zim_engine_workloads_sdk::{
    Capability, CapabilitySet, IntegrationManifest, RuntimeKindRef, WorkloadIntegration,
};

pub use handle::GenericContainerWorkload;
pub use lifecycle::{lifecycle_plan, GenericContainerLifecyclePlan};
pub use negotiate::{
    full_runtime, minimal_runtime, negotiate, negotiate_or_reject, PlacementVerdict,
};
pub use spec::{default_spec, GenericContainerSpec, ResourceLimitsSpec, SpecEndpoint};

/// The integration's declaration, validated at first access.
#[doc(hidden)]
pub static MANIFEST: LazyLock<IntegrationManifest> = LazyLock::new(|| {
    IntegrationManifest::builtin(
        zim_engine_domain::WorkloadKind::GenericContainer,
        "org.zaminmc.workload.generic_container",
        "0.1.0",
        "Generic Container",
        "Arbitrary containerized workloads with placement-time capability negotiation.",
        // The kind's own posture: the manifest requires nothing —
        // the spec's own declarations are the requirements, and
        // the negotiation is per-placement (§4).
        CapabilitySet::EMPTY,
        Capability::ALL.into_iter().collect(),
        &[RuntimeKindRef::Container, RuntimeKindRef::Remote],
    )
});

/// The GenericContainer integration's singleton.
pub struct GenericContainerIntegration;

impl WorkloadIntegration for GenericContainerIntegration {
    fn manifest(&self) -> &'static IntegrationManifest {
        &MANIFEST
    }

    /// Validate a workload row's spec against the
    /// GenericContainer schema.
    fn validate_spec(&self, spec: &Value) -> Result<(), SpecError> {
        let parsed: GenericContainerSpec = serde_json::from_value(spec.clone())
            .map_err(|e| SpecError::Unparsable(e.to_string()))?;
        parsed.validate()
    }

    /// The kind's default spec: a busybox long-sleep container
    /// with no requirements (the minimal generic workload).
    fn default_spec(&self) -> Value {
        serde_json::to_value(default_spec()).expect("the default generic spec is serializable")
    }

    /// The kind's documented no-install posture: there is no
    /// first-run installation for a generic container (the
    /// container is its own source of truth), so the hook
    /// answers `Unsupported` with the reason instead of
    /// inventing a step.
    fn prepare_install(&self, _spec: &Value) -> Result<Value, IntegrationError> {
        Err(IntegrationError::unsupported(
            "org.zaminmc.workload.generic_container",
            "prepare_install",
        ))
    }

    /// The lifecycle translation: the transparent power actions.
    fn lifecycle_plan(&self, action: &str) -> Result<Value, IntegrationError> {
        let plan = lifecycle_plan(&default_spec(), action)?;
        serde_json::to_value(&plan)
            .map_err(|e| IntegrationError::Spec(SpecError::Unparsable(e.to_string())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_declares_the_negotiated_contract() {
        let integration = GenericContainerIntegration;
        assert_eq!(
            integration.kind(),
            zim_engine_domain::WorkloadKind::GenericContainer
        );
        let manifest = integration.manifest();
        assert_eq!(manifest.id, "org.zaminmc.workload.generic_container");
        assert!(manifest.validate().is_ok());
        assert!(manifest.accepts_runtime(RuntimeKindRef::Container));
        assert!(manifest.accepts_runtime(RuntimeKindRef::Remote));
        // The kind's own posture: nothing required, everything
        // optional (the spec's declarations are the requirements).
        assert_eq!(manifest.required_capabilities.len(), 0);
        assert_eq!(manifest.optional_capabilities.len(), Capability::ALL.len());
    }

    #[test]
    fn the_integration_validates_specs() {
        let integration = GenericContainerIntegration;
        integration
            .validate_spec(&integration.default_spec())
            .unwrap();

        let empty = serde_json::json!({ "image": "" });
        assert!(integration.validate_spec(&empty).is_err());

        let unknown_capability = serde_json::json!({
            "image": "busybox:1.36",
            "required_capabilities": ["TelepathyCapable"]
        });
        assert!(integration.validate_spec(&unknown_capability).is_err());
    }

    #[test]
    fn the_install_hook_answers_the_documented_no_install_posture() {
        let integration = GenericContainerIntegration;
        let err = integration
            .prepare_install(&integration.default_spec())
            .unwrap_err();
        assert!(matches!(err, IntegrationError::Unsupported { .. }));
    }

    #[test]
    fn the_lifecycle_hooks_speak_the_transparent_vocabulary() {
        let integration = GenericContainerIntegration;
        for action in ["start", "stop", "restart", "kill"] {
            let plan = integration.lifecycle_plan(action).unwrap();
            let parsed: GenericContainerLifecyclePlan = serde_json::from_value(plan).unwrap();
            assert_eq!(parsed.action, action);
            assert!(!parsed.requires_install);
        }
        let err = integration.lifecycle_plan("hibernate").unwrap_err();
        assert!(matches!(err, IntegrationError::Unsupported { .. }));
    }
}
