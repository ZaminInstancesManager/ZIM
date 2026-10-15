//! Lifecycle plans (M19): what each power action *means* for a
//! DevProcess workload.
//!
//! The engine's supervision layer owns the generic lifecycle; the
//! integration contributes the kind-specific translation. The
//! dev-process translation's own vocabulary:
//!
//! - **Stop is the signal path** — SIGTERM reaches the dev
//!   process through the container's PID 1 (a `cargo watch`
//!   passes it to the child; a plain process dies with it).
//! - **The §6 extended state is `Watching`** — the file-watcher
//!   is active while the policy watches ([`crate::watch`]); the
//!   restart-on-change path is the kind's own lifecycle family,
//!   not a crash.
//! - **No install, no backup** — the kind's documented posture
//!   (§5: "no install/backup expectations"): the process is its
//!   own source of truth, and the data directory is the user's
//!   own tree.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::time::Duration;

use serde::{Deserialize, Serialize};

use zim_engine_workloads_sdk::errors::IntegrationError;

use crate::spec::DevProcessSpec;

/// The default graceful-stop deadline (the engine's 30-second
/// posture; a dev process's own cleanup — the toolchain's
/// shutdown hooks, the test teardowns — fits comfortably).
pub const DEFAULT_STOP_DEADLINE: Duration = Duration::from_secs(30);

/// The §6 extended lifecycle state a dev process surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DevProcessExtendedState {
    /// The base lifecycle (no watcher active — the policy does
    /// not watch).
    None,
    /// The file-watcher is active (the §6 green/Running family
    /// with the "watching for file changes" tooltip).
    Watching,
}

impl DevProcessExtendedState {
    /// Whether the watcher is the active state.
    pub fn is_watching(self) -> bool {
        matches!(self, Self::Watching)
    }
}

/// What one power action translates to for this workload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DevProcessLifecyclePlan {
    /// The action this plan answers.
    pub action: String,
    /// The graceful deadline before the forcible backstop.
    pub graceful_deadline_secs: u64,
    /// Whether the action requires an install first (never, for
    /// this kind — the documented no-install posture).
    pub requires_install: bool,
    /// The extended lifecycle state the action runs through.
    pub extended_state: DevProcessExtendedState,
    /// The restart policy's semantics, in words.
    pub restart_semantics: String,
    /// Human-readable notes the engine may surface.
    pub notes: Vec<String>,
}

/// Build the lifecycle plan for one action against one spec.
///
/// Unknown actions answer
/// [`IntegrationError::Unsupported`][IntegrationError::Unsupported].
pub fn lifecycle_plan(
    spec: &DevProcessSpec,
    action: &str,
) -> Result<DevProcessLifecyclePlan, IntegrationError> {
    let restart_semantics = spec.restart_policy.semantics().to_owned();
    let extended = if spec.restart_policy.watches() {
        DevProcessExtendedState::Watching
    } else {
        DevProcessExtendedState::None
    };
    match action {
        "start" => Ok(DevProcessLifecyclePlan {
            action: action.to_owned(),
            graceful_deadline_secs: DEFAULT_STOP_DEADLINE.as_secs(),
            requires_install: false,
            extended_state: extended,
            restart_semantics: restart_semantics.clone(),
            notes: vec![
                "there is no install expectation for this kind: the \
                 process is its own source of truth (the documented \
                 no-install posture)"
                    .to_owned(),
                "the console stream carries the process's stdout/stderr \
                 from the first line (the dev loop's own output)"
                    .to_owned(),
            ],
        }),
        "stop" => Ok(DevProcessLifecyclePlan {
            action: action.to_owned(),
            graceful_deadline_secs: DEFAULT_STOP_DEADLINE.as_secs(),
            requires_install: false,
            extended_state: DevProcessExtendedState::None,
            restart_semantics,
            notes: Vec::new(),
        }),
        "restart" => Ok(DevProcessLifecyclePlan {
            action: action.to_owned(),
            graceful_deadline_secs: DEFAULT_STOP_DEADLINE.as_secs(),
            requires_install: false,
            extended_state: extended,
            restart_semantics: restart_semantics.clone(),
            notes: vec![
                "restart is stop-then-start: the toolchain's own shutdown \
                 hooks (the test teardowns, the watch children) complete \
                 before the relaunch"
                    .to_owned(),
                "the console history resets with the restart (a fresh \
                 generation)"
                    .to_owned(),
            ],
        }),
        "kill" => Ok(DevProcessLifecyclePlan {
            action: action.to_owned(),
            graceful_deadline_secs: 0,
            requires_install: false,
            extended_state: DevProcessExtendedState::None,
            restart_semantics: restart_semantics.clone(),
            notes: vec!["kill skips the orderly path: the toolchain's children \
                 reset with the container; the sources are untouched \
                 (they live in the bind-mounted data directory)"
                .to_owned()],
        }),
        "watch-restart" => Ok(DevProcessLifecyclePlan {
            action: action.to_owned(),
            graceful_deadline_secs: DEFAULT_STOP_DEADLINE.as_secs(),
            requires_install: false,
            extended_state: DevProcessExtendedState::Watching,
            restart_semantics,
            notes: vec![
                "a watched file changed: the process restarts (debounced \
                 — a burst of changes coalesces into one restart)"
                    .to_owned(),
                "this is the kind's own lifecycle family, not a crash: \
                 the crash-loop protection does not count it"
                    .to_owned(),
            ],
        }),
        _ => Err(IntegrationError::unsupported(
            "org.zaminmc.workload.dev_process",
            "lifecycle_plan",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::default_spec;
    use crate::spec::DevRestartPolicy;

    #[test]
    fn a_watching_policy_surfaces_the_watching_state() {
        let spec = default_spec();
        assert!(spec.restart_policy.watches());
        let plan = lifecycle_plan(&spec, "start").unwrap();
        assert!(plan.extended_state.is_watching());
        assert!(plan.restart_semantics.contains("debounced"));
    }

    #[test]
    fn a_never_policy_surfaces_the_base_lifecycle() {
        let spec = DevProcessSpec {
            command: vec!["sleep".to_owned(), "1000".to_owned()],
            working_directory: None,
            env: Vec::new(),
            watch_patterns: Vec::new(),
            restart_policy: DevRestartPolicy::Never,
            debounce_ms: None,
        };
        let plan = lifecycle_plan(&spec, "start").unwrap();
        assert!(!plan.extended_state.is_watching());
    }

    #[test]
    fn stop_is_the_signal_path() {
        let spec = default_spec();
        let plan = lifecycle_plan(&spec, "stop").unwrap();
        assert_eq!(plan.graceful_deadline_secs, 30);
        assert!(!plan.requires_install);
        assert!(!plan.extended_state.is_watching());
    }

    #[test]
    fn the_watch_restart_is_not_a_crash() {
        let spec = default_spec();
        let plan = lifecycle_plan(&spec, "watch-restart").unwrap();
        assert!(plan.extended_state.is_watching());
        assert!(plan.notes[1].contains("not a crash"));
    }

    #[test]
    fn the_no_install_posture_is_documented() {
        let spec = default_spec();
        for action in ["start", "stop", "restart", "kill", "watch-restart"] {
            let plan = lifecycle_plan(&spec, action).unwrap();
            assert!(!plan.requires_install, "{action}");
        }
        let plan = lifecycle_plan(&spec, "start").unwrap();
        assert!(plan.notes[0].contains("no install expectation"));
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
        let plan = lifecycle_plan(&spec, "watch-restart").unwrap();
        let json = serde_json::to_value(&plan).unwrap();
        let back: DevProcessLifecyclePlan = serde_json::from_value(json).unwrap();
        assert_eq!(back, plan);
    }
}
