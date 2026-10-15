//! Placement-time capability negotiation (M19).
//!
//! §4's enforcement model, the GenericContainer kind's own
//! headline: **"capability negotiation at placement time —
//! whatever the spec declares is what the runtime must
//! provide."** The workload's spec carries its requirements
//! (explicit plus content-implied, per
//! [`GenericContainerSpec::derived_requirements`]); the runtime
//! reports the capabilities it provides; the negotiation is the
//! subset test.
//!
//! A placement that fails reports the missing capabilities —
//! explicitly, not silently: the verdict's `missing` list names
//! every one, and the failing form
//! ([`negotiate_or_reject`]) raises the SDK's
//! `CapabilityMissing` error vocabulary (the §4 "runtime does
//! not provide X" surface), the same error the runtime-side
//! dynamic dispatch would raise for a missing trait object.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use zim_engine_workloads_sdk::errors::IntegrationError;
use zim_engine_workloads_sdk::{Capability, CapabilitySet};

use crate::spec::GenericContainerSpec;

/// A negotiated placement: the verdict plus its inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementVerdict {
    /// Whether the runtime satisfies the workload's requirements.
    pub placed: bool,
    /// The workload's derived requirements (the negotiation's
    /// demand side).
    pub required: CapabilitySet,
    /// The runtime's provided capabilities (the negotiation's
    /// supply side).
    pub provided: CapabilitySet,
    /// The requirements the runtime does not provide (empty when
    /// `placed`).
    pub missing: Vec<Capability>,
}

impl PlacementVerdict {
    /// The verdict's human form (the surfaced placement reason).
    pub fn reason(&self) -> String {
        if self.placed {
            format!(
                "the runtime provides all {} required capabilities",
                self.required.len()
            )
        } else {
            let missing: Vec<String> = self.missing.iter().map(|c| c.to_string()).collect();
            format!(
                "the runtime does not provide {} of the {} required \
                 capabilities: {}",
                self.missing.len(),
                self.required.len(),
                missing.join(", ")
            )
        }
    }
}

/// Negotiate one placement: the spec's derived requirements
/// against the runtime's provided set.
pub fn negotiate(spec: &GenericContainerSpec, provided: &CapabilitySet) -> PlacementVerdict {
    let required = spec.derived_requirements();
    let missing: Vec<Capability> = Capability::ALL
        .into_iter()
        .filter(|capability| required.contains(*capability) && !provided.contains(*capability))
        .collect();
    let placed = missing.is_empty();
    PlacementVerdict {
        placed,
        required,
        provided: *provided,
        missing,
    }
}

/// Negotiate one placement, raising the SDK's structured
/// `CapabilityMissing` vocabulary on failure (the §4 placement
/// failure's error form).
pub fn negotiate_or_reject(
    spec: &GenericContainerSpec,
    provided: &CapabilitySet,
) -> Result<CapabilitySet, IntegrationError> {
    let verdict = negotiate(spec, provided);
    if verdict.placed {
        return Ok(verdict.required);
    }
    Err(IntegrationError::CapabilityMissing {
        capability: verdict.missing[0],
    })
}

/// The capability set a runtime that provides *everything* the
/// vocabulary names (the reference Docker posture: console,
/// readiness, health, filesystem, network, snapshot, migration,
/// exec, metrics).
pub fn full_runtime() -> CapabilitySet {
    Capability::ALL.into_iter().collect()
}

/// The capability set a minimal runtime provides (the base
/// `Runtime` trait's own surface: exec and metrics, per
/// RUST_ENGINE.md §4's minimum).
pub fn minimal_runtime() -> CapabilitySet {
    [Capability::ExecCapable, Capability::MetricsCapable]
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::default_spec;

    fn spec_with(capabilities: &[Capability]) -> GenericContainerSpec {
        GenericContainerSpec {
            required_capabilities: capabilities.to_vec(),
            ..default_spec()
        }
    }

    #[test]
    fn a_satisfied_placement_places() {
        let spec = spec_with(&[Capability::ExecCapable, Capability::MetricsCapable]);
        let verdict = negotiate(&spec, &minimal_runtime());
        assert!(verdict.placed);
        assert!(verdict.missing.is_empty());
        assert_eq!(verdict.reason(), "the runtime provides all 2 required capabilities");
    }

    #[test]
    fn a_deficient_placement_names_every_missing_capability() {
        let spec = spec_with(&[
            Capability::ExecCapable,
            Capability::ConsoleCapable,
            Capability::SnapshotCapable,
        ]);
        let verdict = negotiate(&spec, &minimal_runtime());
        assert!(!verdict.placed);
        assert_eq!(verdict.missing.len(), 2);
        assert!(verdict.missing.contains(&Capability::ConsoleCapable));
        assert!(verdict.missing.contains(&Capability::SnapshotCapable));
        assert!(verdict.reason().contains("ConsoleCapable"));
        assert!(verdict.reason().contains("SnapshotCapable"));
        // The missing capabilities come out in declaration order
        // (the vocabulary's own order, a stable surface).
        assert_eq!(
            verdict.missing[0],
            Capability::ConsoleCapable,
            "{:?}",
            verdict.missing
        );
    }

    #[test]
    fn the_content_implied_requirements_negotiate_too() {
        let mut spec = default_spec();
        spec.health_check = Some(zim_engine_workloads_sdk::HealthCheckSpec::tcp("http"));
        // A minimal runtime cannot take it.
        let verdict = negotiate(&spec, &minimal_runtime());
        assert!(!verdict.placed);
        assert_eq!(verdict.missing, vec![Capability::HealthCheckCapable]);
        // A runtime with the health surface can.
        let mut provided = minimal_runtime();
        provided.insert(Capability::HealthCheckCapable);
        let verdict = negotiate(&spec, &provided);
        assert!(verdict.placed);
    }

    #[test]
    fn a_requirement_free_spec_places_anywhere() {
        let spec = default_spec();
        let verdict = negotiate(&spec, &CapabilitySet::EMPTY);
        assert!(verdict.placed);
        assert_eq!(verdict.reason(), "the runtime provides all 0 required capabilities");
    }

    #[test]
    fn the_rejecting_form_raises_the_structured_vocabulary() {
        let spec = spec_with(&[Capability::SnapshotCapable]);
        let err = negotiate_or_reject(&spec, &minimal_runtime()).unwrap_err();
        match err {
            IntegrationError::CapabilityMissing { capability } => {
                assert_eq!(capability, Capability::SnapshotCapable);
            }
            other => panic!("expected CapabilityMissing, got {other:?}"),
        }
        // The satisfied form returns the requirements.
        let required = negotiate_or_reject(&spec, &full_runtime()).unwrap();
        assert!(required.contains(Capability::SnapshotCapable));
    }

    #[test]
    fn the_reference_postures_cover_the_vocabulary() {
        let full = full_runtime();
        for capability in Capability::ALL {
            assert!(full.contains(capability));
        }
        assert_eq!(full.len(), Capability::ALL.len());
        let minimal = minimal_runtime();
        assert_eq!(minimal.len(), 2);
    }
}
