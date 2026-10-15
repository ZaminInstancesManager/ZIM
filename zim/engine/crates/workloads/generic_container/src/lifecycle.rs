//! Lifecycle plans (M19): what each power action *means* for a
//! GenericContainer workload.
//!
//! The engine's supervision layer owns the generic lifecycle;
//! the integration contributes the kind-specific translation —
//! and for the generic kind, the translation is deliberately
//! *transparent*: there is no kind-specific convention to
//! contribute. The container is the user's own; its stop
//! convention is whatever its image's PID 1 does with SIGTERM;
//! its readiness is its health check's own verdict (or the
//! process's presence when the spec carries no check).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::time::Duration;

use serde::{Deserialize, Serialize};

use zim_engine_workloads_sdk::errors::IntegrationError;

use crate::spec::GenericContainerSpec;

/// The default graceful-stop deadline (the engine's 30-second
/// posture; the image's own shutdown behavior — graceful or not
/// — is the user's choice, and the forcible backstop bounds it).
pub const DEFAULT_STOP_DEADLINE: Duration = Duration::from_secs(30);

/// What one power action translates to for this workload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenericContainerLifecyclePlan {
    /// The action this plan answers.
    pub action: String,
    /// The graceful deadline before the forcible backstop.
    pub graceful_deadline_secs: u64,
    /// Whether the action requires an install first (never, for
    /// this kind — the container is its own source of truth).
    pub requires_install: bool,
    /// The image's stop semantics, in words.
    pub stop_semantics: String,
    /// Human-readable notes the engine may surface.
    pub notes: Vec<String>,
}

/// Build the lifecycle plan for one action against one spec.
///
/// Unknown actions answer
/// [`IntegrationError::Unsupported`][IntegrationError::Unsupported].
pub fn lifecycle_plan(
    _spec: &GenericContainerSpec,
    action: &str,
) -> Result<GenericContainerLifecyclePlan, IntegrationError> {
    let stop_semantics = "SIGTERM reaches the image's own PID 1 — whatever the \
             image's entrypoint does with it is the stop convention; \
             the forcible backstop bounds it"
        .to_owned();
    match action {
        "start" => Ok(GenericContainerLifecyclePlan {
            action: action.to_owned(),
            graceful_deadline_secs: DEFAULT_STOP_DEADLINE.as_secs(),
            requires_install: false,
            stop_semantics: stop_semantics.clone(),
            notes: vec![
                "there is no install expectation for this kind: the \
                 container is its own source of truth"
                    .to_owned(),
                "the health verdict is the health check's own when the \
                 spec carries one, the process's presence otherwise"
                    .to_owned(),
            ],
        }),
        "stop" => Ok(GenericContainerLifecyclePlan {
            action: action.to_owned(),
            graceful_deadline_secs: DEFAULT_STOP_DEADLINE.as_secs(),
            requires_install: false,
            stop_semantics,
            notes: Vec::new(),
        }),
        "restart" => Ok(GenericContainerLifecyclePlan {
            action: action.to_owned(),
            graceful_deadline_secs: DEFAULT_STOP_DEADLINE.as_secs(),
            requires_install: false,
            stop_semantics: stop_semantics.clone(),
            notes: vec![
                "restart is stop-then-start: the image's own shutdown \
                 behavior completes before the relaunch"
                    .to_owned(),
            ],
        }),
        "kill" => Ok(GenericContainerLifecyclePlan {
            action: action.to_owned(),
            graceful_deadline_secs: 0,
            requires_install: false,
            stop_semantics: "kill skips the image's own shutdown: the \
                     container's processes are SIGKILLed; the writable \
                     layers are the image's own story"
                .to_owned(),
            notes: Vec::new(),
        }),
        _ => Err(IntegrationError::unsupported(
            "org.zaminmc.workload.generic_container",
            "lifecycle_plan",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::default_spec;

    #[test]
    fn the_translation_is_transparent() {
        let spec = default_spec();
        for action in ["start", "stop", "restart"] {
            let plan = lifecycle_plan(&spec, action).unwrap();
            assert_eq!(plan.graceful_deadline_secs, 30);
            assert!(!plan.requires_install);
            assert!(plan.stop_semantics.contains("the image's own"));
        }
        let plan = lifecycle_plan(&spec, "kill").unwrap();
        assert_eq!(plan.graceful_deadline_secs, 0);
        assert!(plan.stop_semantics.contains("SIGKILLed"));
    }

    #[test]
    fn the_health_posture_is_documented() {
        let spec = default_spec();
        let plan = lifecycle_plan(&spec, "start").unwrap();
        assert!(plan.notes[1].contains("the health check's own"));
        assert!(plan.notes[1].contains("the process's presence"));
    }

    #[test]
    fn unknown_actions_are_unsupported() {
        let spec = default_spec();
        let err = lifecycle_plan(&spec, "hibernate").unwrap_err();
        assert!(matches!(err, IntegrationError::Unsupported { .. }));
    }

    #[test]
    fn the_plans_round_trip_through_serde() {
        let spec = default_spec();
        let plan = lifecycle_plan(&spec, "start").unwrap();
        let json = serde_json::to_value(&plan).unwrap();
        let back: GenericContainerLifecyclePlan = serde_json::from_value(json).unwrap();
        assert_eq!(back, plan);
    }
}
