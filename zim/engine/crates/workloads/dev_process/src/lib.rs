//! The DevProcess workload integration (M19).
//!
//! Declared per the §5 contract (identity, kind, capabilities,
//! runtime classes) in the hand-written form — the SDK example's
//! "Form 2" (the same posture the Database kind took in M18 and
//! the Website kind takes in M19).
//!
//! The M19 behavioral surface — the roadmap's §M19 dev-process
//! scope:
//!
//! - [`DevProcessSpec`] parsing (the command, the working
//!   directory, the environment, the watch patterns, the restart
//!   policy: `Never` / `OnFileChange` / `Always`);
//! - [`lifecycle_plan`](DevProcessIntegration::lifecycle_plan) —
//!   the signal-path power translations plus the
//!   `watch-restart` family's own plan (the §6 `Watching`
//!   extended state; "not a crash" is the kind's documented
//!   posture);
//! - [`DevProcessWorkload`] — the per-workload §4 capability
//!   handle (console, metrics) over a live runtime, plus the
//!   `notify`-backed file-watch restart supervision;
//! - **no install, no backup** — the kind's documented §5
//!   posture: [`prepare_install`] answers `Unsupported` with
//!   the reason, rather than inventing an install step the kind
//!   does not have.
//!
//! ```text
//! id      = org.zaminmc.workload.dev_process
//! kind    = DevProcess
//! requires: ConsoleCapable
//! optional: FilesystemCapable, MetricsCapable
//! runtimes: Container, Host
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod console;
pub mod handle;
pub mod lifecycle;
pub mod spec;
pub mod watch;

use serde_json::Value;
use std::sync::LazyLock;

use zim_engine_workloads_sdk::errors::{IntegrationError, SpecError};
use zim_engine_workloads_sdk::{
    Capability, IntegrationManifest, RuntimeKindRef, WorkloadIntegration,
};

pub use console::ConsoleBuffer;
pub use handle::DevProcessWorkload;
pub use lifecycle::{lifecycle_plan, DevProcessExtendedState, DevProcessLifecyclePlan};
pub use spec::{default_spec, DevProcessSpec, DevRestartPolicy, EnvBinding};
pub use watch::{glob_matches, FileWatcher, RestartRequest, WatchState};

/// The integration's declaration, validated at first access.
#[doc(hidden)]
pub static MANIFEST: LazyLock<IntegrationManifest> = LazyLock::new(|| {
    IntegrationManifest::builtin(
        zim_engine_domain::WorkloadKind::DevProcess,
        "org.zaminmc.workload.dev_process",
        "0.1.0",
        "Dev Process",
        "Long-running development processes (cargo watch, npm run dev, ...).",
        [Capability::ConsoleCapable].into_iter().collect(),
        // The filesystem (the file-watch restart's own read side)
        // and the metrics sample stay optional in the placement
        // contract (a Host placement may not expose the file
        // surface), the same posture the M16 skeleton declared.
        [Capability::FilesystemCapable, Capability::MetricsCapable]
            .into_iter()
            .collect(),
        &[RuntimeKindRef::Container, RuntimeKindRef::Host],
    )
});

/// The DevProcess integration's singleton.
pub struct DevProcessIntegration;

impl WorkloadIntegration for DevProcessIntegration {
    fn manifest(&self) -> &'static IntegrationManifest {
        &MANIFEST
    }

    /// Validate a workload row's spec against the DevProcess
    /// schema.
    fn validate_spec(&self, spec: &Value) -> Result<(), SpecError> {
        let parsed: DevProcessSpec = serde_json::from_value(spec.clone())
            .map_err(|e| SpecError::Unparsable(e.to_string()))?;
        parsed.validate()
    }

    /// The kind's default spec: the canonical `cargo watch -x
    /// run` dev loop.
    fn default_spec(&self) -> Value {
        serde_json::to_value(default_spec()).expect("the default dev-process spec is serializable")
    }

    /// The kind's documented no-install posture: there is no
    /// first-run installation for a dev process (the process is
    /// its own source of truth), so the hook answers
    /// `Unsupported` with the reason instead of inventing a
    /// step.
    fn prepare_install(&self, _spec: &Value) -> Result<Value, IntegrationError> {
        Err(IntegrationError::unsupported(
            "org.zaminmc.workload.dev_process",
            "prepare_install",
        ))
    }

    /// The lifecycle translation: the signal-path power actions
    /// plus the watch-restart family's own.
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
    fn the_manifest_declares_the_dev_process_contract() {
        let integration = DevProcessIntegration;
        assert_eq!(
            integration.kind(),
            zim_engine_domain::WorkloadKind::DevProcess
        );
        let manifest = integration.manifest();
        assert_eq!(manifest.id, "org.zaminmc.workload.dev_process");
        assert!(manifest.validate().is_ok());
        assert!(manifest.accepts_runtime(RuntimeKindRef::Container));
        assert!(manifest.accepts_runtime(RuntimeKindRef::Host));
        assert!(manifest
            .required_capabilities
            .contains(Capability::ConsoleCapable));
        assert!(manifest
            .optional_capabilities
            .contains(Capability::MetricsCapable));
    }

    #[test]
    fn the_integration_validates_specs() {
        let integration = DevProcessIntegration;
        integration.validate_spec(&integration.default_spec()).unwrap();

        let empty = serde_json::json!({ "command": [] });
        assert!(integration.validate_spec(&empty).is_err());

        let escaping = serde_json::json!({
            "command": ["make"],
            "working_directory": "/etc"
        });
        assert!(integration.validate_spec(&escaping).is_err());
    }

    #[test]
    fn the_install_hook_answers_the_documented_no_install_posture() {
        let integration = DevProcessIntegration;
        let err = integration
            .prepare_install(&integration.default_spec())
            .unwrap_err();
        assert!(matches!(err, IntegrationError::Unsupported { .. }));
    }

    #[test]
    fn the_lifecycle_hooks_speak_the_dev_process_vocabulary() {
        let integration = DevProcessIntegration;
        for action in ["start", "stop", "restart", "kill", "watch-restart"] {
            let plan = integration.lifecycle_plan(action).unwrap();
            let parsed: DevProcessLifecyclePlan = serde_json::from_value(plan).unwrap();
            assert_eq!(parsed.action, action);
            assert!(!parsed.requires_install);
        }
        let err = integration.lifecycle_plan("hibernate").unwrap_err();
        assert!(matches!(err, IntegrationError::Unsupported { .. }));
    }
}
