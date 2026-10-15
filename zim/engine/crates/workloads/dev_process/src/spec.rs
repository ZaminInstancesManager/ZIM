//! The DevProcess workload spec (M19).
//!
//! Per `docs/architecture/WORKLOAD_ABSTRACTION.md` §3, a
//! `Workload` row of kind `DevProcess` carries this spec as its
//! `spec` JSON. The spec is the dev-loop's own shape: the
//! command, the working directory, the environment, the watch
//! patterns, and the restart policy.
//!
//! Unlike the GameServer/Database/Website kinds, the spec
//! carries **no image** — the image is the `Instance` row's own
//! field (a dev process is "whatever toolchain container the
//! user placed"), which is the §3 posture: the kind's fields
//! that don't apply stay null in the Instance and the spec
//! carries the kind's own decisions.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use serde::{Deserialize, Serialize};

use zim_engine_workloads_sdk::errors::SpecError;

/// The default debounce window (the burst-coalescing interval —
/// an editor's save-all writes several files in one burst, and
/// the watcher restarts once, not per file).
pub const DEFAULT_DEBOUNCE_MS: u64 = 500;

/// The restart policy (§3's `DevRestartPolicy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DevRestartPolicy {
    /// Never restart on file change (the plain long-running
    /// process).
    #[default]
    Never,
    /// Restart when a watched file changes (the dev loop: `cargo
    /// watch`, `npm run dev`).
    OnFileChange,
    /// Restart whenever the process exits, file changes or not
    /// (the always-supervise posture).
    Always,
}

impl DevRestartPolicy {
    /// Whether the policy needs the file watcher at all.
    pub fn watches(self) -> bool {
        matches!(self, Self::OnFileChange)
    }

    /// The policy's semantics, in words.
    pub fn semantics(self) -> &'static str {
        match self {
            Self::Never => {
                "the process runs until it exits or is stopped; \
                     file changes restart nothing"
            }
            Self::OnFileChange => {
                "the process restarts when a watched file \
                             changes (debounced)"
            }
            Self::Always => "the process restarts whenever it exits",
        }
    }
}

/// One environment binding, spec-shaped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvBinding {
    /// The variable's name.
    pub name: String,
    /// The variable's value.
    pub value: String,
}

impl EnvBinding {
    /// Construct one binding.
    pub fn new(name: &str, value: &str) -> Self {
        Self {
            name: name.to_owned(),
            value: value.to_owned(),
        }
    }
}

/// The DevProcess workload spec.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DevProcessSpec {
    /// The command to run (argv form; e.g. `cargo watch -x run`
    /// arrives as `["cargo", "watch", "-x", "run"]`).
    pub command: Vec<String>,
    /// The working directory, relative to the workload's data
    /// root (`None` = the data root itself).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    /// The process's environment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<EnvBinding>,
    /// The file-change patterns that trigger a restart (glob
    /// form: `**/*.rs`, `src/**/*.js`; relative to the working
    /// directory).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watch_patterns: Vec<String>,
    /// The restart policy.
    #[serde(default)]
    pub restart_policy: DevRestartPolicy,
    /// The debounce window (milliseconds) — how long the watcher
    /// waits before a burst of changes coalesces into one
    /// restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debounce_ms: Option<u64>,
}

impl DevProcessSpec {
    /// The debounce window (the spec's override or the default).
    pub fn debounce(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.debounce_ms.unwrap_or(DEFAULT_DEBOUNCE_MS))
    }

    /// The effective watch patterns (the spec's own; a watching
    /// policy with no patterns watches everything).
    pub fn effective_watch_patterns(&self) -> Vec<String> {
        if self.watch_patterns.is_empty() {
            vec!["**/*".to_owned()]
        } else {
            self.watch_patterns.clone()
        }
    }

    /// Validate the spec's structural invariants.
    ///
    /// Strict where the runtime must not launch garbage (an
    /// empty command, a working directory that escapes the data
    /// root, a malformed environment name); permissive where the
    /// process's own runtime surfaces the failure (the command's
    /// argv passes through verbatim — a missing binary is the
    /// container's error, not a schema violation).
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.command.is_empty() {
            return Err(SpecError::Invalid {
                field: "command".to_owned(),
                message: "the dev process command must not be empty".to_owned(),
            });
        }
        if self.command.iter().any(|arg| arg.is_empty()) {
            return Err(SpecError::Invalid {
                field: "command".to_owned(),
                message: "the command carries an empty argument".to_owned(),
            });
        }
        if let Some(dir) = &self.working_directory {
            if dir.is_empty() {
                return Err(SpecError::Invalid {
                    field: "working_directory".to_owned(),
                    message: "the working directory must not be empty".to_owned(),
                });
            }
            if dir.starts_with('/') || dir.split('/').any(|part| part == "..") {
                return Err(SpecError::Invalid {
                    field: "working_directory".to_owned(),
                    message: "the working directory must be relative to the \
                         workload's data root (no leading slash, no parent hops)"
                        .to_owned(),
                });
            }
        }
        let mut names = std::collections::BTreeSet::new();
        for binding in &self.env {
            if binding.name.is_empty() {
                return Err(SpecError::Invalid {
                    field: "env".to_owned(),
                    message: "an environment binding has an empty name".to_owned(),
                });
            }
            if binding.name.contains('=') {
                return Err(SpecError::Invalid {
                    field: "env".to_owned(),
                    message: format!(
                        "the environment name {:?} carries '=' (it is not a name)",
                        binding.name
                    ),
                });
            }
            if !names.insert(binding.name.clone()) {
                return Err(SpecError::Invalid {
                    field: "env".to_owned(),
                    message: format!("the environment name {:?} is declared twice", binding.name),
                });
            }
        }
        for pattern in &self.watch_patterns {
            if pattern.trim().is_empty() {
                return Err(SpecError::Invalid {
                    field: "watch_patterns".to_owned(),
                    message: "a watch pattern is empty".to_owned(),
                });
            }
        }
        Ok(())
    }
}

/// The default spec: the canonical `cargo watch` dev loop.
pub fn default_spec() -> DevProcessSpec {
    DevProcessSpec {
        command: vec![
            "cargo".to_owned(),
            "watch".to_owned(),
            "-x".to_owned(),
            "run".to_owned(),
        ],
        working_directory: None,
        env: Vec::new(),
        watch_patterns: vec!["**/*.rs".to_owned()],
        restart_policy: DevRestartPolicy::OnFileChange,
        debounce_ms: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_spec_validates_and_watches() {
        let spec = default_spec();
        spec.validate().unwrap();
        assert!(spec.restart_policy.watches());
        assert_eq!(spec.effective_watch_patterns(), vec!["**/*.rs"]);
        assert_eq!(spec.debounce(), std::time::Duration::from_millis(500));
    }

    #[test]
    fn an_empty_command_is_refused() {
        let mut spec = default_spec();
        spec.command = Vec::new();
        assert!(spec.validate().is_err());
        spec.command = vec!["cargo".to_owned(), String::new()];
        assert!(spec.validate().is_err());
    }

    #[test]
    fn a_escaping_working_directory_is_refused() {
        let mut spec = default_spec();
        spec.working_directory = Some("/etc".to_owned());
        assert!(spec.validate().is_err());
        spec.working_directory = Some("a/../..".to_owned());
        assert!(spec.validate().is_err());
        spec.working_directory = Some("project".to_owned());
        spec.validate().unwrap();
    }

    #[test]
    fn the_environment_shapes_are_validated() {
        let mut spec = default_spec();
        spec.env = vec![EnvBinding::new("", "v")];
        assert!(spec.validate().is_err());
        spec.env = vec![EnvBinding::new("A=B", "v")];
        assert!(spec.validate().is_err());
        spec.env = vec![EnvBinding::new("A", "1"), EnvBinding::new("A", "2")];
        assert!(spec.validate().is_err());
        spec.env = vec![EnvBinding::new("A", "1")];
        spec.validate().unwrap();
    }

    #[test]
    fn an_empty_watch_pattern_is_refused() {
        let mut spec = default_spec();
        spec.watch_patterns = vec![String::new()];
        assert!(spec.validate().is_err());
    }

    #[test]
    fn a_watching_policy_without_patterns_watches_everything() {
        let spec = DevProcessSpec {
            command: vec!["npm".to_owned(), "run".to_owned(), "dev".to_owned()],
            working_directory: None,
            env: Vec::new(),
            watch_patterns: Vec::new(),
            restart_policy: DevRestartPolicy::OnFileChange,
            debounce_ms: Some(250),
        };
        spec.validate().unwrap();
        assert_eq!(spec.effective_watch_patterns(), vec!["**/*"]);
        assert_eq!(spec.debounce(), std::time::Duration::from_millis(250));
    }

    #[test]
    fn the_policies_speak_their_semantics() {
        assert!(DevRestartPolicy::Never
            .semantics()
            .contains("file changes restart nothing"));
        assert!(DevRestartPolicy::OnFileChange
            .semantics()
            .contains("debounced"));
        assert!(DevRestartPolicy::Always
            .semantics()
            .contains("whenever it exits"));
        assert!(!DevRestartPolicy::Never.watches());
        assert!(DevRestartPolicy::OnFileChange.watches());
        assert!(!DevRestartPolicy::Always.watches());
    }

    #[test]
    fn the_spec_round_trips_through_serde() {
        let spec = default_spec();
        let json = serde_json::to_value(&spec).unwrap();
        let back: DevProcessSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, spec);

        let json = serde_json::json!({ "command": ["make", "watch"] });
        let spec: DevProcessSpec = serde_json::from_value(json).unwrap();
        spec.validate().unwrap();
        // The omitted policy defaults to never (serde's default).
        assert_eq!(spec.restart_policy, DevRestartPolicy::Never);
        assert!(spec.effective_watch_patterns().contains(&"**/*".to_owned()));
    }
}
