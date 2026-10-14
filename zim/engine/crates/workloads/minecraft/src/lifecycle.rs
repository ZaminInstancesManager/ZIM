//! Lifecycle plans (M17): what each power action *means* for a
//! GameServer workload.
//!
//! The engine's supervision layer owns the generic four-state
//! lifecycle; the integration contributes the kind-specific
//! translation — most importantly **stop**: the Minecraft
//! convention is the console line `stop` (the server saves its
//! worlds and exits cleanly), *not* a SIGTERM. Wings's
//! `server.power` behavior for the default egg family is exactly
//! this: `StopConfiguration.Command` is written to the console,
//! then the daemon waits for the process to exit before the
//! forcible backstop. A signal-shaped stop config (`^C`) routes to
//! the runtime's graceful-stop path instead.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::time::Duration;

use serde::{Deserialize, Serialize};

use zim_engine_workloads_sdk::errors::IntegrationError;

use crate::spec::{GameServerSpec, StopCommand};

/// The default graceful-stop deadline (Wings's `system` stop
/// timeout default: 30 seconds before the forcible backstop).
pub const DEFAULT_STOP_DEADLINE: Duration = Duration::from_secs(30);

/// What one power action translates to for this workload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LifecyclePlan {
    /// The action this plan answers (`"start"`, `"stop"`,
    /// `"restart"`, `"kill"`).
    pub action: String,
    /// How the stop is performed, when the action stops the
    /// workload (`None` for start/kill).
    pub stop: Option<StopCommand>,
    /// The graceful deadline before the forcible backstop.
    pub graceful_deadline_secs: u64,
    /// Whether the action requires the workload to be installed
    /// first.
    pub requires_install: bool,
    /// Human-readable notes the engine may surface.
    pub notes: Vec<String>,
}

/// Build the lifecycle plan for one action against one spec.
///
/// Unknown actions answer
/// [`IntegrationError::Unsupported`][IntegrationError::Unsupported]
/// (the SDK hook's standard refusal).
pub fn lifecycle_plan(
    spec: &GameServerSpec,
    action: &str,
) -> Result<LifecyclePlan, IntegrationError> {
    match action {
        "start" => Ok(LifecyclePlan {
            action: action.to_owned(),
            stop: None,
            graceful_deadline_secs: DEFAULT_STOP_DEADLINE.as_secs(),
            requires_install: true,
            notes: vec![
                "the startup command renders strictly: an unconfigured variable fails \
                 before launch"
                    .to_owned(),
            ],
        }),
        "stop" => Ok(LifecyclePlan {
            action: action.to_owned(),
            stop: Some(spec.stop.clone()),
            graceful_deadline_secs: DEFAULT_STOP_DEADLINE.as_secs(),
            requires_install: false,
            notes: stop_notes(&spec.stop),
        }),
        "restart" => Ok(LifecyclePlan {
            action: action.to_owned(),
            stop: Some(spec.stop.clone()),
            graceful_deadline_secs: DEFAULT_STOP_DEADLINE.as_secs(),
            requires_install: false,
            notes: {
                let mut notes = stop_notes(&spec.stop);
                notes.push(
                    "restart is stop-then-start: the world save completes \
                            before the relaunch"
                        .to_owned(),
                );
                notes
            },
        }),
        "kill" => Ok(LifecyclePlan {
            action: action.to_owned(),
            stop: None,
            graceful_deadline_secs: 0,
            requires_install: false,
            notes: vec![
                "kill is immediate: unsaved world state since the last save is \
                 lost (the Minecraft convention treats stop, not kill, as the \
                 orderly path)"
                    .to_owned(),
            ],
        }),
        _ => Err(IntegrationError::unsupported(
            "org.zaminmc.workload.minecraft",
            unsupported_action(),
        )),
    }
}

/// The static operation name for an unsupported action (the error
/// vocabulary's `operation` field is `'static`; the refused action
/// itself rides the error's `Display`, which names the operation).
fn unsupported_action() -> &'static str {
    "lifecycle_plan"
}

/// The notes a stop-carrying plan attaches.
fn stop_notes(stop: &StopCommand) -> Vec<String> {
    if stop.signal {
        vec![
            "the egg declares a signal-shaped stop: the runtime's graceful stop \
             (SIGTERM-or-equivalent) is used instead of console input"
                .to_owned(),
        ]
    } else {
        vec![format!(
            "the console line {:?} is sent and the process exit is awaited \
                 before the forcible backstop (the Minecraft save-and-exit \
                 convention)",
            stop.command
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::GameServerSpec;
    use zim_engine_workloads_sdk::WorkloadIntegration;

    fn fixture_spec() -> GameServerSpec {
        let report = zim_engine_eggs::import_legacy_json(
            r##"{
                "meta": { "version": "PTDL_v1", "update_url": null },
                "name": "Paper Minecraft",
                "description": "A high performance Minecraft server.",
                "author": "support@pterodactyl.io",
                "features": ["eula"],
                "docker_images": { "Java 21": "eclipse-temurin:21-jre" },
                "startup": "java -Xms128M -Xmx{{SERVER_MEMORY}}M -jar {{SERVER_JARFILE}}",
                "config": {
                    "files": "{}",
                    "startup": { "done": "Done ([0-9.]+s)! For help, type \"help\"" },
                    "stop": { "command": "end", "signal": false }
                },
                "variables": []
            }"##,
            &zim_engine_eggs::ImportPolicy::default(),
        )
        .expect("import fixture egg");
        GameServerSpec::from_egg(&report.egg, None).expect("spec")
    }

    #[test]
    fn stop_carries_the_egg_command_and_the_save_semantics_note() {
        let spec = fixture_spec();
        let plan = lifecycle_plan(&spec, "stop").unwrap();
        assert_eq!(plan.stop.as_ref().unwrap().command, "end");
        assert!(!plan.stop.as_ref().unwrap().signal);
        assert_eq!(plan.graceful_deadline_secs, 30);
        assert!(plan.notes[0].contains("end"));
        assert!(plan.notes[0].contains("save"));
    }

    #[test]
    fn the_default_stop_is_the_minecraft_convention() {
        // An egg without a stop config gets the `stop` console line.
        let report = zim_engine_eggs::import_legacy_json(
            r##"{
                "meta": { "version": "PTDL_v1", "update_url": null },
                "name": "Vanilla",
                "description": "",
                "author": "a",
                "docker_images": { "Java 21": "eclipse-temurin:21-jre" },
                "startup": "java -jar server.jar",
                "config": {},
                "variables": []
            }"##,
            &zim_engine_eggs::ImportPolicy::default(),
        )
        .expect("import fixture egg");
        let spec = GameServerSpec::from_egg(&report.egg, None).unwrap();
        let plan = lifecycle_plan(&spec, "stop").unwrap();
        assert_eq!(plan.stop.as_ref().unwrap().command, "stop");
    }

    #[test]
    fn signal_shaped_stops_route_to_the_runtime_path() {
        let mut spec = fixture_spec();
        spec.stop = StopCommand {
            command: "^C".to_owned(),
            signal: true,
        };
        let plan = lifecycle_plan(&spec, "stop").unwrap();
        assert!(plan.stop.as_ref().unwrap().signal);
        assert!(plan.notes[0].contains("SIGTERM"));
    }

    #[test]
    fn start_requires_install_and_kill_is_immediate() {
        let spec = fixture_spec();
        let start = lifecycle_plan(&spec, "start").unwrap();
        assert!(start.requires_install);
        assert!(start.stop.is_none());
        let kill = lifecycle_plan(&spec, "kill").unwrap();
        assert_eq!(kill.graceful_deadline_secs, 0);
        assert!(!kill.requires_install);
        assert!(kill.notes[0].contains("lost"));
    }

    #[test]
    fn restart_notes_the_save_before_relaunch() {
        let spec = fixture_spec();
        let plan = lifecycle_plan(&spec, "restart").unwrap();
        assert_eq!(plan.action, "restart");
        assert!(plan.stop.is_some());
        assert!(plan.notes.iter().any(|n| n.contains("stop-then-start")));
    }

    #[test]
    fn unknown_actions_are_unsupported() {
        let spec = fixture_spec();
        let err = lifecycle_plan(&spec, "hibernate").unwrap_err();
        assert!(matches!(err, IntegrationError::Unsupported { .. }));
        // The singleton hook surfaces the same refusal (the hook is
        // this function over the parsed spec).
        let hook = crate::MinecraftIntegration;
        let value_err = hook
            .lifecycle_plan("hibernate")
            .expect_err("unsupported action");
        assert!(matches!(value_err, IntegrationError::Unsupported { .. }));
        assert_eq!(hook.id(), "org.zaminmc.workload.minecraft");
    }
}
