//! The Minecraft Server workload integration (M17).
//!
//! Declared per the §5 contract (identity, kind, capabilities,
//! runtime classes) in the hand-written form — the SDK example's
//! "Form 2", the shape an integration takes when it outgrows the
//! declaration-only macro form (the macro generates the trait impl
//! with `Unsupported` hooks; M17 owns the hooks, so the impl is
//! hand-written against the same generated-manifest shape).
//!
//! The M17 behavioral surface — the Pterodactyl-egg parity the
//! roadmap's §M17 demands:
//!
//! - [`GameServerSpec`] parsing from an imported Pterodactyl egg
//!   (the legacy JSON importer and the ZIM-native `zim.toml` form
//!   both land in the same domain [`Egg`] shape; the spec snapshots
//!   the behavioral bits);
//! - [`prepare_install`](MinecraftIntegration::prepare_install) —
//!   the EULA write, the config-file rewrite rules, and the egg's
//!   install script, as an ordered plan;
//! - [`lifecycle_plan`](MinecraftIntegration::lifecycle_plan) — the
//!   Minecraft stop convention (the console line `stop`, the process
//!   exit awaited, the forcible backstop) and the start/restart/kill
//!   translations;
//! - [`MinecraftWorkload`] — the per-workload §4 capability handle
//!   (console, filesystem, exec, metrics, migration) over a live
//!   runtime.
//!
//! ```text
//! id      = org.zaminmc.workload.minecraft
//! kind    = GameServer
//! requires: ConsoleCapable, FilesystemCapable, MetricsCapable, ExecCapable
//! optional: Migratable, SnapshotCapable
//! runtimes: Container, Remote
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod console;
pub mod handle;
pub mod install;
pub mod lifecycle;
pub mod spec;

use serde_json::Value;
use std::sync::LazyLock;

use zim_engine_workloads_sdk::errors::{IntegrationError, SpecError};
use zim_engine_workloads_sdk::{
    Capability, IntegrationManifest, RuntimeKindRef, WorkloadIntegration,
};

pub use console::ConsoleBuffer;
pub use handle::{MinecraftWorkload, StopOutcome};
pub use install::{apply_file_steps, build_install_plan, InstallPlan, InstallStep, EULA_FILE};
pub use lifecycle::{lifecycle_plan, LifecyclePlan, DEFAULT_STOP_DEADLINE};
pub use spec::{ConfigFileRule, GameServerSpec, InstallScript, StopCommand, VariableBinding};

/// The integration's declaration, validated at first access.
#[doc(hidden)]
pub static MANIFEST: LazyLock<IntegrationManifest> = LazyLock::new(|| {
    IntegrationManifest::builtin(
        zim_engine_domain::WorkloadKind::GameServer,
        "org.zaminmc.workload.minecraft",
        "0.1.0",
        "Minecraft Server",
        "Minecraft and Minecraft-compatible game servers.",
        [
            Capability::ConsoleCapable,
            Capability::FilesystemCapable,
            Capability::MetricsCapable,
            Capability::ExecCapable,
        ]
        .into_iter()
        .collect(),
        [Capability::Migratable, Capability::SnapshotCapable]
            .into_iter()
            .collect(),
        &[RuntimeKindRef::Container, RuntimeKindRef::Remote],
    )
});

/// The Minecraft Server integration's singleton.
pub struct MinecraftIntegration;

impl WorkloadIntegration for MinecraftIntegration {
    fn manifest(&self) -> &'static IntegrationManifest {
        &MANIFEST
    }

    /// Validate a workload row's spec against the GameServer schema
    /// (strict where the engine must not launch garbage; permissive
    /// where Pterodactyl is — see [`GameServerSpec::validate`]).
    fn validate_spec(&self, spec: &Value) -> Result<(), SpecError> {
        let parsed: GameServerSpec = serde_json::from_value(spec.clone())
            .map_err(|e| SpecError::Unparsable(e.to_string()))?;
        parsed.validate()
    }

    /// The kind's default spec: a minimal vanilla-shaped form (the
    /// API offers it when a workload is created without one; every
    /// behavioral field carries the Minecraft convention).
    fn default_spec(&self) -> Value {
        serde_json::json!({
            // The nil UUID: an unbound egg (the API's creation form
            // binds a real one at instantiation).
            "egg_id": "00000000-0000-0000-0000-000000000000",
            "egg_name": "Minecraft Server",
            "image": "eclipse-temurin:21-jre",
            "image_label": null,
            "startup": "java -Xms128M -Xmx{{SERVER_MEMORY}}M -jar {{SERVER_JARFILE}}",
            "variables": [
                {
                    "env_name": "SERVER_JARFILE",
                    "value": "server.jar",
                    "default_value": "server.jar",
                    "user_viewable": true,
                    "user_editable": true,
                    "rules": ["required"]
                },
                {
                    "env_name": "SERVER_MEMORY",
                    "value": "512",
                    "default_value": "512",
                    "user_viewable": true,
                    "user_editable": true,
                    "rules": ["required"]
                }
            ],
            "allocations": [],
            "skip_egg_scripts": false,
            "crash_detection_enabled": true,
            "eula_accepted": false,
            "eula_handled_by_installer": false,
            "startup_done": "Done ([0-9.]+s)! For help, type \"help\"",
            "stop": { "command": "stop", "signal": false },
            "config_files": [],
            "install_script": null
        })
    }

    /// The first-run install plan: the egg's install script, the
    /// config-file rewrite rules, and the EULA write (§M17's
    /// "EULA acceptance + JAR bootstrap" — the JAR itself is the
    /// install script's job, which the plan references).
    fn prepare_install(&self, spec: &Value) -> Result<Value, IntegrationError> {
        let parsed: GameServerSpec = serde_json::from_value(spec.clone())
            .map_err(|e| SpecError::Unparsable(e.to_string()))?;
        parsed.validate()?;
        let plan = build_install_plan(&parsed);
        serde_json::to_value(&plan)
            .map_err(|e| IntegrationError::Spec(SpecError::Unparsable(e.to_string())))
    }

    /// The lifecycle translation for one action (`start`, `stop`,
    /// `restart`, `kill`); unknown actions answer `Unsupported`.
    ///
    /// The spec-less call answers the convention-defaulted form
    /// (the API's "what would stop do here" preview); spec-carrying
    /// callers use [`lifecycle_plan`] directly.
    fn lifecycle_plan(&self, action: &str) -> Result<Value, IntegrationError> {
        let spec = GameServerSpec {
            egg_id: zim_engine_domain::EggId::new(),
            egg_name: String::new(),
            image: "placeholder:local".to_owned(),
            image_label: None,
            startup: "java -jar server.jar".to_owned(),
            variables: Vec::new(),
            allocations: Vec::new(),
            skip_egg_scripts: false,
            crash_detection_enabled: true,
            eula_accepted: false,
            eula_handled_by_installer: false,
            startup_done: None,
            stop: StopCommand::default(),
            config_files: Vec::new(),
            install_script: None,
        };
        let plan = lifecycle_plan(&spec, action)?;
        serde_json::to_value(&plan)
            .map_err(|e| IntegrationError::Spec(SpecError::Unparsable(e.to_string())))
    }
}

/// The canonical crate name.
pub const CRATE_NAME: &str = "zim_engine_workloads_minecraft";

#[cfg(test)]
mod tests {
    use super::*;
    use zim_engine_domain::WorkloadKind;
    use zim_engine_workloads_sdk::{Capability, RuntimeKindRef};

    #[test]
    fn declares_the_minecraft_contract() {
        assert_eq!(MinecraftIntegration.kind(), WorkloadKind::GameServer);
        let manifest = MinecraftIntegration.manifest();
        assert_eq!(manifest.id, "org.zaminmc.workload.minecraft");
        assert_eq!(manifest.display_name, "Minecraft Server");
        assert!(manifest.validate().is_ok());
        assert!(manifest.accepts_runtime(RuntimeKindRef::Container));
        assert!(manifest.accepts_runtime(RuntimeKindRef::Remote));
        assert!(manifest
            .required_capabilities
            .contains(Capability::ConsoleCapable));
        assert!(manifest
            .required_capabilities
            .contains(Capability::FilesystemCapable));
        assert!(manifest
            .required_capabilities
            .contains(Capability::MetricsCapable));
        assert!(manifest
            .required_capabilities
            .contains(Capability::ExecCapable));
        assert!(manifest
            .optional_capabilities
            .contains(Capability::Migratable));
        assert!(manifest
            .optional_capabilities
            .contains(Capability::SnapshotCapable));
        assert_eq!(
            MinecraftIntegration.runtime_requirements(),
            manifest.required_capabilities
        );
    }

    #[test]
    fn the_behavioral_hooks_are_no_longer_unsupported() {
        // M17's contract: the hooks answer real values, not the
        // M16 defaults.
        let mut default_spec = MinecraftIntegration.default_spec();
        MinecraftIntegration
            .validate_spec(&default_spec)
            .expect("default validates");

        // The EULA step appears once the spec accepts it.
        default_spec["eula_accepted"] = serde_json::json!(true);
        let plan_value = MinecraftIntegration
            .prepare_install(&default_spec)
            .expect("prepare_install is implemented");
        let plan: InstallPlan = serde_json::from_value(plan_value).unwrap();
        assert!(plan.steps.iter().any(|step| matches!(
            step,
            InstallStep::WriteFile { path, .. } if path == "eula.txt"
        )));

        let lifecycle = MinecraftIntegration
            .lifecycle_plan("stop")
            .expect("lifecycle_plan is implemented");
        let plan: LifecyclePlan = serde_json::from_value(lifecycle).unwrap();
        assert_eq!(plan.stop.as_ref().unwrap().command, "stop");
    }

    #[test]
    fn a_garbage_spec_is_refused_at_validation() {
        assert!(MinecraftIntegration.validate_spec(&Value::Null).is_err());
        assert!(MinecraftIntegration
            .validate_spec(&serde_json::json!({ "image": "" }))
            .is_err());
        let err = MinecraftIntegration
            .prepare_install(&Value::Null)
            .unwrap_err();
        assert!(err.to_string().contains("spec"), "{err}");
    }

    #[test]
    fn the_default_spec_round_trips_into_the_typed_form() {
        let value = MinecraftIntegration.default_spec();
        let spec: GameServerSpec = serde_json::from_value(value).unwrap();
        spec.validate().unwrap();
        assert_eq!(spec.stop.command, "stop");
        assert_eq!(spec.image, "eclipse-temurin:21-jre");
        assert_eq!(spec.variables.len(), 2);
        // The default spec ships with the strict-render-relevant
        // variables pre-bound.
        let rendered = spec.render_startup().unwrap();
        assert!(rendered.contains("-Xmx512M"));
        assert!(rendered.contains("server.jar"));
    }

    #[test]
    fn crate_name_is_set() {
        assert_eq!(CRATE_NAME, "zim_engine_workloads_minecraft");
    }
}
