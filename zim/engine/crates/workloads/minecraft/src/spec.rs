//! The GameServer workload spec (M17).
//!
//! Per `docs/architecture/WORKLOAD_ABSTRACTION.md` §3, a `Workload`
//! row of kind `GameServer` carries this spec as its §10.15 `spec`
//! JSON. The spec is the *user-facing* half (image choice, startup
//! template, variable bindings, EULA decision) plus a snapshot of
//! the imported egg's behavioral bits (stop command, startup-done
//! regex, config parser rules, install script) — the snapshot is
//! what lets the integration's hooks
//! ([`prepare_install`][crate::MinecraftIntegration::prepare_install],
//! [`lifecycle_plan`][crate::MinecraftIntegration::lifecycle_plan])
//! operate from the spec alone, without a live egg row.
//!
//! [`GameServerSpec::from_egg`] is the Pterodactyl-parity entry
//! point: an imported legacy egg (or a ZIM-native one — both land
//! in the same [`Egg`] domain shape through
//! `zim_engine_eggs`) becomes a spec with defaults filled in.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::BTreeMap;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use zim_engine_domain::{
    AllocationId, CommandTemplate, DockerImage, Egg, EggId, EggImage, ParserKind, ReplaceRule,
    StopConfig,
};
use zim_engine_workloads_sdk::errors::SpecError;

/// One variable binding: the egg's declaration fused with the
/// instance's override.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VariableBinding {
    /// The environment variable name (uppercase, the legacy
    /// convention).
    pub env_name: String,
    /// The effective value (the instance's override, else the egg's
    /// default).
    pub value: String,
    /// The egg's default value.
    pub default_value: String,
    /// Whether users may see this variable.
    pub user_viewable: bool,
    /// Whether users may override this variable per instance.
    pub user_editable: bool,
    /// The legacy validation rules, verbatim.
    pub rules: Vec<String>,
}

/// How this game server is stopped (the egg's stop config with the
/// Minecraft convention as the default).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopCommand {
    /// The stop command: a console line to send (`stop` — the
    /// Minecraft convention Wings's own default egg family uses) or
    /// a signal name when `signal` is set.
    pub command: String,
    /// Whether `command` is a signal (`^C`) rather than console
    /// input.
    pub signal: bool,
}

impl Default for StopCommand {
    fn default() -> Self {
        // The Minecraft convention: the server's console `stop`
        // command saves worlds and exits cleanly. Wings's
        // `server.power` sends exactly this for the default eggs.
        Self {
            command: "stop".to_owned(),
            signal: false,
        }
    }
}

impl From<&StopConfig> for StopCommand {
    fn from(config: &StopConfig) -> Self {
        Self {
            command: config.command.clone(),
            signal: config.signal,
        }
    }
}

/// One config-file rewrite rule carried from the egg (the domain's
/// `FileParserRule`, spec-shaped).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigFileRule {
    /// The file's path relative to the data directory.
    pub file: String,
    /// The parser kind.
    pub parser: ParserKind,
    /// The find/replace pairs (values are `{{PLACEHOLDER}}`
    /// templates rendered at apply time).
    pub replace: Vec<ReplaceRule>,
}

/// The install script snapshot (the egg's install script, carried
/// so `prepare_install` can plan its execution without a live egg
/// row; the installs job handler (M8.3) executes it in its
/// constrained container).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallScript {
    /// The container image the script runs in.
    pub container: String,
    /// The entrypoint (`bash` in virtually all legacy eggs).
    pub entrypoint: String,
    /// The script body, verbatim.
    pub script: String,
}

/// The GameServer workload spec.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameServerSpec {
    /// The egg this workload instantiates.
    pub egg_id: EggId,
    /// The egg's display name (diagnostics; not behavioral).
    pub egg_name: String,
    /// The docker image the workload runs under.
    pub image: String,
    /// The image's label in the egg's image map (diagnostics).
    pub image_label: Option<String>,
    /// The startup command template (`{{PLACEHOLDER}}` shape).
    pub startup: String,
    /// The variable bindings.
    pub variables: Vec<VariableBinding>,
    /// The instance's allocation ids (the primary allocation rides
    /// the Instance row; per §3 these are the additional ones).
    #[serde(default)]
    pub allocations: Vec<AllocationId>,
    /// Whether the egg's install/update scripts are skipped.
    #[serde(default)]
    pub skip_egg_scripts: bool,
    /// Whether crash detection is enabled for this workload.
    #[serde(default = "default_true")]
    pub crash_detection_enabled: bool,
    /// Whether the Minecraft EULA is accepted (`eula.txt` is written
    /// at install time).
    #[serde(default)]
    pub eula_accepted: bool,
    /// Whether the egg's installer handles the EULA itself (the
    /// legacy `eula` feature flag).
    #[serde(default)]
    pub eula_handled_by_installer: bool,
    /// The regex that marks the server as started (the egg's
    /// `config.startup.done`; console-output detection).
    pub startup_done: Option<String>,
    /// How the workload is stopped.
    pub stop: StopCommand,
    /// The config-file rewrite rules applied at install time.
    #[serde(default)]
    pub config_files: Vec<ConfigFileRule>,
    /// The install script snapshot (when the egg carries one).
    #[serde(default)]
    pub install_script: Option<InstallScript>,
}

/// The `crash_detection_enabled` default (serde's default fn).
fn default_true() -> bool {
    true
}

impl GameServerSpec {
    /// Build a spec from an imported egg, choosing the image by
    /// `image_label` (or the egg's first image when no label is
    /// given).
    ///
    /// The behavioral snapshot (stop, startup-done, parser rules,
    /// install script) is copied from the egg; variable bindings
    /// start at the egg's defaults.
    pub fn from_egg(egg: &Egg, image_label: Option<&str>) -> Result<Self, SpecError> {
        let image = Self::pick_image(egg, image_label)?;
        Ok(Self {
            egg_id: egg.id,
            egg_name: egg.name.clone(),
            image: image.image.to_string(),
            image_label: Some(image.label.clone()),
            startup: egg.startup.as_str().to_owned(),
            variables: egg
                .variables
                .iter()
                .map(|v| VariableBinding {
                    env_name: v.env_name.clone(),
                    value: v.default_value.clone(),
                    default_value: v.default_value.clone(),
                    user_viewable: v.user_viewable,
                    user_editable: v.user_editable,
                    rules: v.rules.clone(),
                })
                .collect(),
            allocations: Vec::new(),
            skip_egg_scripts: false,
            crash_detection_enabled: true,
            eula_accepted: false,
            eula_handled_by_installer: egg.features.eula,
            startup_done: egg.config.startup_done.clone(),
            stop: egg
                .config
                .stop
                .as_ref()
                .map(StopCommand::from)
                .unwrap_or_default(),
            config_files: egg
                .config
                .files
                .iter()
                .map(|rule| ConfigFileRule {
                    file: rule.file.clone(),
                    parser: rule.parser.clone(),
                    replace: rule.replace.clone(),
                })
                .collect(),
            install_script: egg.scripts.install.as_ref().map(|script| InstallScript {
                container: script.container.to_string(),
                entrypoint: script.entrypoint.clone(),
                script: script.script.clone(),
            }),
        })
    }

    /// Choose the egg image for a label (or the first one).
    fn pick_image<'a>(egg: &'a Egg, label: Option<&'a str>) -> Result<&'a EggImage, SpecError> {
        if egg.images.is_empty() {
            return Err(SpecError::Invalid {
                field: "image".to_owned(),
                message: "the egg declares no docker images; a GameServer workload \
                          cannot be instantiated from it"
                    .to_owned(),
            });
        }
        match label {
            Some(label) => egg
                .images
                .iter()
                .find(|image| image.label == label)
                .ok_or_else(|| SpecError::Invalid {
                    field: "image_label".to_owned(),
                    message: format!("the egg has no image labeled {label:?}"),
                }),
            None => egg.images.first().ok_or_else(|| SpecError::Invalid {
                field: "image".to_owned(),
                message: "the egg declares no docker images".to_owned(),
            }),
        }
    }

    /// Validate the spec's structural invariants.
    ///
    /// Deliberately permissive where Pterodactyl is (an unaccepted
    /// EULA is a runtime failure, not a schema violation) and strict
    /// where the engine must not launch garbage (empty image,
    /// empty startup, malformed variable names, an empty stop
    /// command).
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.image.trim().is_empty() {
            return Err(SpecError::Invalid {
                field: "image".to_owned(),
                message: "the workload image must not be empty".to_owned(),
            });
        }
        if DockerImage::from_str(&self.image).is_err() {
            return Err(SpecError::Invalid {
                field: "image".to_owned(),
                message: format!("{:?} is not a valid image reference", self.image),
            });
        }
        if self.startup.trim().is_empty() {
            return Err(SpecError::Invalid {
                field: "startup".to_owned(),
                message: "the startup command must not be empty".to_owned(),
            });
        }
        for binding in &self.variables {
            if binding.env_name.is_empty() {
                return Err(SpecError::Invalid {
                    field: "variables".to_owned(),
                    message: "a variable binding has an empty env name".to_owned(),
                });
            }
            if !binding
                .env_name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            {
                return Err(SpecError::Invalid {
                    field: "variables".to_owned(),
                    message: format!(
                        "variable name {:?} is not the uppercase legacy form",
                        binding.env_name
                    ),
                });
            }
        }
        if self.stop.command.trim().is_empty() {
            return Err(SpecError::Invalid {
                field: "stop".to_owned(),
                message: "the stop command must not be empty".to_owned(),
            });
        }
        if let Some(startup_done) = &self.startup_done {
            if regex::Regex::new(startup_done).is_err() {
                return Err(SpecError::Invalid {
                    field: "startup_done".to_owned(),
                    message: format!("{startup_done:?} is not a valid regex"),
                });
            }
        }
        Ok(())
    }

    /// The variable bindings as a render context.
    pub fn variable_map(&self) -> BTreeMap<String, String> {
        self.variables
            .iter()
            .map(|binding| (binding.env_name.clone(), binding.value.clone()))
            .collect()
    }

    /// Render the startup command strictly (unknown placeholders are
    /// errors — a mis-configured workload fails before launch, not
    /// with a literal `{{SERVER_JARFILE}}` in its argv).
    pub fn render_startup(&self) -> Result<String, SpecError> {
        CommandTemplate::new(self.startup.clone())
            .render(&self.variable_map())
            .map_err(|e| SpecError::Invalid {
                field: "startup".to_owned(),
                message: e.to_string(),
            })
    }

    /// Apply one instance-level variable override.
    ///
    /// Only variables the egg declared editable may be overridden —
    /// the Panel's `user_editable` gate, enforced integration-side.
    pub fn override_variable(&mut self, env_name: &str, value: &str) -> Result<(), SpecError> {
        let binding = self
            .variables
            .iter_mut()
            .find(|binding| binding.env_name == env_name)
            .ok_or_else(|| SpecError::Invalid {
                field: "variables".to_owned(),
                message: format!("the egg declares no variable {env_name:?}"),
            })?;
        if !binding.user_editable {
            return Err(SpecError::Invalid {
                field: "variables".to_owned(),
                message: format!("variable {env_name:?} is not user-editable"),
            });
        }
        binding.value = value.to_owned();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn egg_fixture() -> Egg {
        // The eggs crate's importer does the JSON legwork; here the
        // domain shape is assembled directly (the from_egg mapping
        // is under test, not the import).
        zim_engine_eggs::import_legacy_json(
            r##"{
                "meta": { "version": "PTDL_v1", "update_url": null },
                "name": "Paper Minecraft",
                "description": "A high performance Minecraft server.",
                "author": "support@pterodactyl.io",
                "features": ["eula"],
                "docker_images": { "Java 21": "eclipse-temurin:21-jre", "Java 17": "eclipse-temurin:17-jre" },
                "startup": "java -Xms128M -Xmx{{SERVER_MEMORY}}M -jar {{SERVER_JARFILE}}",
                "config": {
                    "files": "{\"server.properties\": {\"parser\": \"properties\", \"replace\": [{\"match\": \"server-port\", \"value\": \"{{SERVER_PORT}}\"}]}}",
                    "startup": { "done": "Done ([0-9.]+s)! For help, type \"help\"" },
                    "stop": { "command": "stop", "signal": false }
                },
                "scripts": {
                    "installation": {
                        "script": "#!/bin/bash\ncd /mnt/server\ncurl -o server.jar ${DOWNLOAD_URL}\n",
                        "container": "debian:12-slim",
                        "entrypoint": "bash"
                    }
                },
                "variables": [
                    {
                        "name": "Server Jar File",
                        "description": "The jar to run",
                        "env_variable": "SERVER_JARFILE",
                        "default_value": "server.jar",
                        "user_viewable": true,
                        "user_editable": true,
                        "rules": ["required", "regex:^[a-zA-Z0-9_-]+\\.jar$"]
                    },
                    {
                        "name": "Download URL",
                        "description": "Where the installer pulls the jar from",
                        "env_variable": "DOWNLOAD_URL",
                        "default_value": "https://example.com/server.jar",
                        "user_viewable": false,
                        "user_editable": false,
                        "rules": ["required"]
                    }
                ]
            }"##,
            &zim_engine_eggs::ImportPolicy::default(),
        )
        .expect("import fixture egg")
        .egg
    }

    #[test]
    fn from_egg_snapshots_the_behavioral_bits() {
        let egg = egg_fixture();
        let spec = GameServerSpec::from_egg(&egg, Some("Java 21")).unwrap();
        assert_eq!(spec.image, "eclipse-temurin:21-jre");
        assert_eq!(spec.image_label.as_deref(), Some("Java 21"));
        assert_eq!(
            spec.startup,
            "java -Xms128M -Xmx{{SERVER_MEMORY}}M -jar {{SERVER_JARFILE}}"
        );
        assert_eq!(spec.variables.len(), 2);
        assert!(spec.eula_handled_by_installer);
        assert_eq!(
            spec.startup_done.as_deref(),
            Some("Done ([0-9.]+s)! For help, type \"help\"")
        );
        assert_eq!(spec.stop.command, "stop");
        assert!(!spec.stop.signal);
        assert_eq!(spec.config_files.len(), 1);
        assert_eq!(spec.config_files[0].file, "server.properties");
        assert_eq!(
            spec.install_script.as_ref().map(|s| s.container.as_str()),
            Some("debian:12-slim")
        );
    }

    #[test]
    fn the_labelless_choice_is_deterministic() {
        // The egg's image map deserializes through serde_json's
        // ordered map, so "first" is the alphabetically-first label
        // ("Java 17" here) — a deterministic, documented default;
        // callers that care pass an explicit label.
        let egg = egg_fixture();
        let spec = GameServerSpec::from_egg(&egg, None).unwrap();
        assert_eq!(spec.image, "eclipse-temurin:17-jre");
        assert_eq!(spec.image_label.as_deref(), Some("Java 17"));
    }

    #[test]
    fn an_unknown_image_label_is_rejected() {
        let egg = egg_fixture();
        let err = GameServerSpec::from_egg(&egg, Some("Java 8")).unwrap_err();
        assert!(err.to_string().contains("Java 8"));
    }

    #[test]
    fn the_stop_default_is_the_minecraft_convention() {
        assert_eq!(StopCommand::default().command, "stop");
        assert!(!StopCommand::default().signal);
    }

    #[test]
    fn validate_accepts_the_fixture() {
        let egg = egg_fixture();
        let mut spec = GameServerSpec::from_egg(&egg, None).unwrap();
        spec.variables.push(VariableBinding {
            env_name: "SERVER_MEMORY".to_owned(),
            value: "1024".to_owned(),
            default_value: "512".to_owned(),
            user_viewable: true,
            user_editable: true,
            rules: vec![],
        });
        spec.validate().unwrap();
        let startup = spec.render_startup().unwrap();
        assert!(startup.contains("-Xmx1024M"));
        assert!(startup.contains("server.jar"));
    }

    #[test]
    fn validate_rejects_the_garbage_shapes() {
        let egg = egg_fixture();
        let mut spec = GameServerSpec::from_egg(&egg, None).unwrap();

        spec.image = String::new();
        assert!(spec.validate().is_err());

        spec.image = "ghcr.io/ parkour/runner:1".to_owned();
        assert!(spec.validate().is_err());

        spec.image = "eclipse-temurin:21-jre".to_owned();
        spec.startup = "   ".to_owned();
        assert!(spec.validate().is_err());

        spec.startup = "java -jar server.jar".to_owned();
        spec.stop.command = String::new();
        assert!(spec.validate().is_err());

        spec.stop = StopCommand::default();
        spec.startup_done = Some("(unbalanced".to_owned());
        assert!(spec.validate().is_err());
    }

    #[test]
    fn a_malformed_variable_name_is_rejected() {
        let egg = egg_fixture();
        let mut spec = GameServerSpec::from_egg(&egg, None).unwrap();
        spec.variables.push(VariableBinding {
            env_name: "lower_case".to_owned(),
            value: "x".to_owned(),
            default_value: "x".to_owned(),
            user_viewable: true,
            user_editable: true,
            rules: vec![],
        });
        assert!(spec.validate().is_err());
    }

    #[test]
    fn rendering_fails_strictly_on_unknown_placeholders() {
        let egg = egg_fixture();
        // SERVER_MEMORY is not declared by the fixture egg.
        let spec = GameServerSpec::from_egg(&egg, None).unwrap();
        let err = spec.render_startup().unwrap_err();
        assert!(err.to_string().contains("SERVER_MEMORY"), "{err}");
    }

    #[test]
    fn only_editable_variables_can_be_overridden() {
        let egg = egg_fixture();
        let mut spec = GameServerSpec::from_egg(&egg, None).unwrap();
        spec.override_variable("SERVER_JARFILE", "paper.jar")
            .unwrap();
        assert_eq!(
            spec.variable_map()
                .get("SERVER_JARFILE")
                .map(String::as_str),
            Some("paper.jar")
        );
        // DOWNLOAD_URL is declared not user-editable.
        let err = spec
            .override_variable("DOWNLOAD_URL", "https://evil")
            .unwrap_err();
        assert!(err.to_string().contains("not user-editable"));
        // An unknown variable name.
        let err = spec.override_variable("NOT_DECLARED", "x").unwrap_err();
        assert!(err.to_string().contains("NOT_DECLARED"));
    }

    #[test]
    fn the_spec_round_trips_through_serde() {
        let egg = egg_fixture();
        let spec = GameServerSpec::from_egg(&egg, None).unwrap();
        let json = serde_json::to_value(&spec).unwrap();
        let back: GameServerSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, spec);
    }
}
