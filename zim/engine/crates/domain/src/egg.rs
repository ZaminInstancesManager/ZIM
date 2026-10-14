//! Egg (server definition) entities.
//!
//! # Source of truth
//!
//! `Egg` and its satellite types preserve the legacy Pterodactyl
//! egg vocabulary per `docs/compatibility/MIGRATION_AND_COMPATIBILITY.md`
//! §3: the startup template's `{{PLACEHOLDER}}` shape, the labeled
//! docker images, the verbatim install scripts, the per-file config
//! parser rules, and the environment-variable declarations. The
//! importer (`zim_engine_eggs`) converts the legacy JSON (and the
//! ZIM-native `zim.toml`) into these rows; the runtime consumes
//! them at startup rendering and install time.
//!
//! The domain layer enforces the structural invariants (non-empty
//! names, uppercase env names, no duplicate variables); semantic
//! checks (security policy, placeholder resolution) are the
//! importer's job.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::id::{EggId, EggVariableId};
use crate::value::{CommandTemplate, DockerImage};

/// A server definition: everything needed to instantiate an
/// instance of a workload type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Egg {
    /// Strongly-typed egg identifier.
    pub id: EggId,
    /// The legacy Pterodactyl egg UUID (PTDL exports carry one;
    /// ZIM-native eggs have none). Used for import deduplication.
    pub legacy_uuid: Option<String>,
    /// Display name (non-empty).
    pub name: String,
    /// Free-form description.
    pub description: String,
    /// Author attribution.
    pub author: String,
    /// The labeled docker images the egg can run under (may be
    /// empty — an image-less egg imports but cannot instantiate).
    pub images: Vec<EggImage>,
    /// The startup command template (`{{PLACEHOLDER}}` shape).
    pub startup: CommandTemplate,
    /// The feature flags (security-relevant).
    pub features: EggFeatures,
    /// The install/update scripts (stored verbatim).
    pub scripts: EggScripts,
    /// The config parser rules + startup/stop behavior.
    pub config: EggConfig,
    /// The declared environment variables.
    pub variables: Vec<EggVariable>,
    /// Row creation time.
    pub created_at: DateTime<Utc>,
    /// Row last-update time.
    pub updated_at: DateTime<Utc>,
}

impl Egg {
    /// Validate the structural invariants (see the module docs).
    pub fn validate(&self) -> Result<(), EggError> {
        if self.name.trim().is_empty() {
            return Err(EggError::EmptyName);
        }
        for image in &self.images {
            if image.label.trim().is_empty() {
                return Err(EggError::EmptyImageLabel);
            }
        }
        if self.startup.as_str().trim().is_empty() {
            return Err(EggError::EmptyStartup);
        }
        let mut seen = Vec::new();
        for variable in &self.variables {
            variable.validate()?;
            if seen.contains(&variable.env_name) {
                return Err(EggError::DuplicateVariable(variable.env_name.clone()));
            }
            seen.push(variable.env_name.clone());
        }
        Ok(())
    }
}

/// One labeled docker image an egg may run under (the legacy
/// `docker_images` map preserves the labels).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EggImage {
    /// The image's label ("Java 21", "Paper 1.20", ...).
    pub label: String,
    /// The image reference.
    pub image: DockerImage,
}

/// The security-relevant egg feature flags (the legacy string array
/// mapped onto ZIM's canonical object form).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EggFeatures {
    /// The egg's installer handles the Minecraft EULA (informational).
    pub eula: bool,
    /// The install script needs privileged (root) execution —
    /// rejected by default at import (Pterodactyl's own spelling of
    /// the flag name is preserved in the docs; the field name here
    /// is spelled correctly).
    pub privileged_install_scripts: bool,
    /// The workload must run as root — rejected by default at
    /// import.
    pub requires_root: bool,
}

/// The install and update scripts (verbatim).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EggScripts {
    /// The installation script (runs in a constrained container at
    /// install time).
    pub install: Option<EggScript>,
    /// The update script (runs on egg updates).
    pub update: Option<EggScript>,
}

/// One script: body + the constrained container it runs in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EggScript {
    /// The container image the script runs in.
    pub container: DockerImage,
    /// The entrypoint (`bash` in virtually all legacy eggs).
    pub entrypoint: String,
    /// The script body, verbatim.
    pub script: String,
}

/// The egg's config: per-file parser rules and process behavior.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EggConfig {
    /// The per-file parser rules (applied at install/start by the
    /// runtime config-rewriting module).
    pub files: Vec<FileParserRule>,
    /// The regex that marks the workload as "started" (console
    /// output detection).
    pub startup_done: Option<String>,
    /// How to stop the workload.
    pub stop: Option<StopConfig>,
}

/// One file's rewriting rule (the legacy `config.files` entry).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileParserRule {
    /// The file's path relative to the instance's data directory.
    pub file: String,
    /// The parser kind.
    pub parser: ParserKind,
    /// The find/replace pairs applied by the parser.
    pub replace: Vec<ReplaceRule>,
}

/// The parser vocabulary (Wings-compatible; unknown legacy kinds
/// are preserved verbatim as [`ParserKind::Other`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParserKind {
    /// `server.properties`-style key=value files.
    Properties,
    /// Whole-file regex rewriting.
    File,
    /// INI-style files.
    Ini,
    /// YAML files.
    Yaml,
    /// JSON files.
    Json,
    /// XML files (the `beevik/etree` approach Wings uses).
    Xml,
    /// An unknown legacy kind, preserved verbatim.
    Other(String),
}

/// One find/replace pair of a parser rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplaceRule {
    /// What to match (a key for structured parsers, a regex for the
    /// `file` parser).
    pub r#match: String,
    /// The replacement (a `{{PLACEHOLDER}}` template).
    pub value: String,
}

/// How the workload is stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopConfig {
    /// The stop command (a console line to send, or a signal name).
    pub command: String,
    /// Whether the command is a signal (`^C`) rather than console
    /// input.
    pub signal: bool,
}

/// One declared environment variable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EggVariable {
    /// Strongly-typed variable identifier.
    pub id: EggVariableId,
    /// The environment variable name (uppercase
    /// `A-Z`/`0-9`/`_`, the legacy convention).
    pub env_name: String,
    /// Display name.
    pub name: String,
    /// Free-form description.
    pub description: String,
    /// The default value (empty string when none).
    pub default_value: String,
    /// Whether the variable is shown to users.
    pub user_viewable: bool,
    /// Whether users may override the value per instance.
    pub user_editable: bool,
    /// The legacy validation rules, verbatim (`"required"`,
    /// `"regex:..."`, ...).
    pub rules: Vec<String>,
}

impl EggVariable {
    /// Validate the structural invariants.
    pub fn validate(&self) -> Result<(), EggError> {
        if self.env_name.is_empty() {
            return Err(EggError::EmptyEnvName);
        }
        if !self
            .env_name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(EggError::MalformedEnvName(self.env_name.clone()));
        }
        Ok(())
    }
}

/// Domain-level validation errors for Egg.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EggError {
    /// The egg name is empty or whitespace-only.
    #[error("egg name must not be empty")]
    EmptyName,
    /// An image label is empty.
    #[error("image labels must not be empty")]
    EmptyImageLabel,
    /// The startup template is empty.
    #[error("egg startup must not be empty")]
    EmptyStartup,
    /// A variable's env name is empty.
    #[error("variable env name must not be empty")]
    EmptyEnvName,
    /// A variable's env name is not uppercase
    /// `A-Z`/`0-9`/`_`.
    #[error("malformed variable env name {0:?}; expected uppercase A-Z, 0-9, _")]
    MalformedEnvName(String),
    /// Two variables share an env name.
    #[error("duplicate variable {0:?}")]
    DuplicateVariable(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variable(env_name: &str) -> EggVariable {
        EggVariable {
            id: EggVariableId::new(),
            env_name: env_name.to_owned(),
            name: "Var".to_owned(),
            description: String::new(),
            default_value: String::new(),
            user_viewable: true,
            user_editable: true,
            rules: Vec::new(),
        }
    }

    fn egg(variables: Vec<EggVariable>) -> Egg {
        Egg {
            id: EggId::new(),
            legacy_uuid: None,
            name: "Test".to_owned(),
            description: String::new(),
            author: String::new(),
            images: Vec::new(),
            startup: CommandTemplate::new("run"),
            features: EggFeatures::default(),
            scripts: EggScripts::default(),
            config: EggConfig::default(),
            variables,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn a_well_formed_egg_validates() {
        egg(vec![variable("SERVER_JARFILE")]).validate().unwrap();
    }

    #[test]
    fn lowercase_env_names_are_rejected() {
        let err = egg(vec![variable("server_jar")]).validate().unwrap_err();
        assert_eq!(err, EggError::MalformedEnvName("server_jar".to_owned()));
    }

    #[test]
    fn duplicate_env_names_are_rejected() {
        let err = egg(vec![variable("A"), variable("A")])
            .validate()
            .unwrap_err();
        assert_eq!(err, EggError::DuplicateVariable("A".to_owned()));
    }

    #[test]
    fn empty_names_are_rejected() {
        let mut subject = egg(vec![]);
        subject.name = "  ".to_owned();
        assert_eq!(subject.validate().unwrap_err(), EggError::EmptyName);
        subject.name = "Test".to_owned();
        subject.startup = CommandTemplate::new("");
        assert_eq!(subject.validate().unwrap_err(), EggError::EmptyStartup);
    }
}
