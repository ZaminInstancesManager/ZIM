//! Lifecycle plans (M19): what each power action *means* for a
//! Website workload.
//!
//! The engine's supervision layer owns the generic lifecycle; the
//! integration contributes the kind-specific translation. The
//! website translation's headline difference from the GameServer
//! kind's: like the Database kind (M18), **there is no console**.
//! A web server's stop convention is the signal path — SIGTERM,
//! which every M19 carrier's entrypoint translates into its own
//! orderly shutdown (nginx's fast shutdown drains its workers;
//! Node and Python processes die with the container's PID 1).
//!
//! The §6 extended states surface here as the plan's
//! `extended_state` vocabulary: `Deploying` (a new version is
//! being deployed) and `Draining` (the old version is refusing
//! new connections) — the deploy family's own states, reported
//! while [`WebsiteWorkload::deploy_async`][crate::WebsiteWorkload::deploy_async]
//! runs.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::time::Duration;

use serde::{Deserialize, Serialize};

use zim_engine_workloads_sdk::errors::IntegrationError;

use crate::spec::WebsiteSpec;

/// The default graceful-stop deadline (the engine's 30-second
/// posture; nginx's worker drain and the application servers'
/// own shutdown hooks fit comfortably inside it).
pub const DEFAULT_STOP_DEADLINE: Duration = Duration::from_secs(30);

/// The §6 extended lifecycle states a website surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebsiteExtendedState {
    /// The base lifecycle (no extended state active).
    None,
    /// A new version is being deployed (§6's yellow/Starting
    /// family).
    Deploying,
    /// The old version is draining after a switch (§6's
    /// yellow/Starting family).
    Draining,
}

impl WebsiteExtendedState {
    /// Whether the state is one of the deploy family's own.
    pub fn is_deploy_family(self) -> bool {
        matches!(self, Self::Deploying | Self::Draining)
    }
}

/// What one power action translates to for this workload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebsiteLifecyclePlan {
    /// The action this plan answers.
    pub action: String,
    /// The graceful deadline before the forcible backstop.
    pub graceful_deadline_secs: u64,
    /// Whether the action requires the workload to be installed
    /// (the site's release structure bootstrapped) first.
    pub requires_install: bool,
    /// The extended lifecycle state the action runs through.
    pub extended_state: WebsiteExtendedState,
    /// The server's stop semantics, in words.
    pub stop_semantics: String,
    /// Human-readable notes the engine may surface.
    pub notes: Vec<String>,
}

/// Build the lifecycle plan for one action against one spec.
///
/// Unknown actions answer
/// [`IntegrationError::Unsupported`][IntegrationError::Unsupported].
pub fn lifecycle_plan(
    spec: &WebsiteSpec,
    action: &str,
) -> Result<WebsiteLifecyclePlan, IntegrationError> {
    let stop_semantics = stop_semantics(spec);
    match action {
        "start" => Ok(WebsiteLifecyclePlan {
            action: action.to_owned(),
            graceful_deadline_secs: DEFAULT_STOP_DEADLINE.as_secs(),
            requires_install: true,
            extended_state: WebsiteExtendedState::None,
            stop_semantics: stop_semantics.clone(),
            notes: vec![
                "the first boot serves the install-time release (the \
                 initial release + current symlink the install path \
                 bootstraps)"
                    .to_owned(),
                "the health check answers Unhealthy until the listener \
                 binds its port"
                    .to_owned(),
            ],
        }),
        "stop" => Ok(WebsiteLifecyclePlan {
            action: action.to_owned(),
            graceful_deadline_secs: DEFAULT_STOP_DEADLINE.as_secs(),
            requires_install: false,
            extended_state: WebsiteExtendedState::None,
            stop_semantics,
            notes: Vec::new(),
        }),
        "restart" => Ok(WebsiteLifecyclePlan {
            action: action.to_owned(),
            graceful_deadline_secs: DEFAULT_STOP_DEADLINE.as_secs(),
            requires_install: false,
            extended_state: WebsiteExtendedState::None,
            stop_semantics: stop_semantics.clone(),
            notes: vec!["restart is stop-then-start: in-flight requests drain \
                 with the stop before the relaunch re-binds the listener"
                .to_owned()],
        }),
        "kill" => Ok(WebsiteLifecyclePlan {
            action: action.to_owned(),
            graceful_deadline_secs: 0,
            requires_install: false,
            extended_state: WebsiteExtendedState::None,
            stop_semantics: format!(
                "kill skips the orderly path: the {} carrier's workers \
                 are SIGKILLed; already-accepted connections reset, and \
                 the next start re-binds the listener",
                spec.server
            ),
            notes: Vec::new(),
        }),
        "deploy" => Ok(WebsiteLifecyclePlan {
            action: action.to_owned(),
            graceful_deadline_secs: DEFAULT_STOP_DEADLINE.as_secs(),
            requires_install: true,
            extended_state: WebsiteExtendedState::Deploying,
            stop_semantics: stop_semantics.clone(),
            notes: vec![
                "the deploy family runs the §M19 sequence: upload, \
                 alongside, switch, drain, verify, retain"
                    .to_owned(),
                "a failed verify leaves the rollback window open (the \
                 previous releases stay on disk)"
                    .to_owned(),
            ],
        }),
        "drain" => Ok(WebsiteLifecyclePlan {
            action: action.to_owned(),
            graceful_deadline_secs: DEFAULT_STOP_DEADLINE.as_secs(),
            requires_install: false,
            extended_state: WebsiteExtendedState::Draining,
            stop_semantics,
            notes: vec!["drain is the deploy's own step (in-flight requests \
                 finish; the static carrier has no long-lived \
                 connections to refuse)"
                .to_owned()],
        }),
        _ => Err(IntegrationError::unsupported(
            "org.zaminmc.workload.website",
            "lifecycle_plan",
        )),
    }
}

/// The server's stop semantics, in words (the plan's
/// `stop_semantics` payload).
fn stop_semantics(spec: &WebsiteSpec) -> String {
    match spec.server {
        crate::server::WebServer::Static => "SIGTERM drives nginx's fast shutdown: workers stop \
             accepting, in-flight requests drain, and the master exits \
             cleanly within the deadline"
            .to_owned(),
        crate::server::WebServer::Node => {
            "SIGTERM reaches the Node process through the container's \
             PID 1; an application with a shutdown hook drains inside \
             the deadline, and the forcible backstop bounds the rest"
                .to_owned()
        }
        crate::server::WebServer::Python => {
            "SIGTERM reaches the Python process through the container's \
             PID 1; an application with a shutdown hook drains inside \
             the deadline, and the forcible backstop bounds the rest"
                .to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::default_spec;

    #[test]
    fn stop_is_the_signal_path_not_a_console_command() {
        let spec = default_spec();
        let plan = lifecycle_plan(&spec, "stop").unwrap();
        assert_eq!(plan.graceful_deadline_secs, 30);
        assert!(plan.stop_semantics.contains("fast shutdown"));
        assert!(!plan.requires_install);
        assert!(!plan.extended_state.is_deploy_family());
    }

    #[test]
    fn start_notes_the_install_time_release() {
        let spec = default_spec();
        let plan = lifecycle_plan(&spec, "start").unwrap();
        assert!(plan.requires_install);
        assert!(plan.notes[0].contains("install-time release"));
        assert!(plan.notes[1].contains("Unhealthy"));
    }

    #[test]
    fn restart_and_kill_carry_their_semantics() {
        let spec = default_spec();
        let plan = lifecycle_plan(&spec, "restart").unwrap();
        assert!(plan.notes.iter().any(|note| note.contains("drain")));
        let plan = lifecycle_plan(&spec, "kill").unwrap();
        assert_eq!(plan.graceful_deadline_secs, 0);
        assert!(plan.stop_semantics.contains("SIGKILLed"));
    }

    #[test]
    fn the_deploy_family_carries_its_extended_states() {
        let spec = default_spec();
        let plan = lifecycle_plan(&spec, "deploy").unwrap();
        assert_eq!(plan.extended_state, WebsiteExtendedState::Deploying);
        assert!(plan.extended_state.is_deploy_family());
        assert!(plan.notes[0].contains("§M19 sequence"));

        let plan = lifecycle_plan(&spec, "drain").unwrap();
        assert_eq!(plan.extended_state, WebsiteExtendedState::Draining);
        assert!(!WebsiteExtendedState::None.is_deploy_family());
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
        let plan = lifecycle_plan(&spec, "deploy").unwrap();
        let json = serde_json::to_value(&plan).unwrap();
        let back: WebsiteLifecyclePlan = serde_json::from_value(json).unwrap();
        assert_eq!(back, plan);
    }
}
