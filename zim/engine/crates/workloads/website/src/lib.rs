//! The Website workload integration (M19).
//!
//! Declared per the §5 contract (identity, kind, capabilities,
//! runtime classes) in the hand-written form — the SDK example's
//! "Form 2", the shape an integration takes when it owns its
//! behavioral hooks (the same posture the Database kind took in
//! M18).
//!
//! The M19 behavioral surface — the roadmap's §M19 website scope:
//!
//! - [`WebsiteSpec`] parsing (the server table's three classes:
//!   static/nginx, node, python; endpoints; the health check; the
//!   HTTPS posture; the deploy retention);
//! - [`prepare_install`](WebsiteIntegration::prepare_install) —
//!   the first-run plan (the engine-written nginx.conf for the
//!   static carrier, the placeholder first release, the current
//!   symlink);
//! - [`lifecycle_plan`](WebsiteIntegration::lifecycle_plan) — the
//!   signal-path stop translation plus the deploy family's own
//!   `Deploying`/`Draining` extended states (§6);
//! - [`WebsiteWorkload`] — the per-workload §4 capability handle
//!   (health check, filesystem, network, metrics) over a live
//!   runtime, plus the deploy/rollback family over the
//!   release-directory layout.
//!
//! ```text
//! id      = org.zaminmc.workload.website
//! kind    = Website
//! requires: HealthCheckCapable, FilesystemCapable, NetworkCapable
//! optional: MetricsCapable, ExecCapable
//! runtimes: Container, Remote
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod deploy;
pub mod handle;
pub mod install;
pub mod lifecycle;
pub mod server;
pub mod spec;

use serde_json::Value;
use std::sync::LazyLock;

use zim_engine_workloads_sdk::errors::{IntegrationError, SpecError};
use zim_engine_workloads_sdk::{
    Capability, IntegrationManifest, RuntimeKindRef, WorkloadIntegration,
};

pub use deploy::{deploy_plan, rollback_plan, DeployStep};
pub use handle::{DeployOutcome, WebsiteWorkload};
pub use install::build_install_plan;
pub use lifecycle::{lifecycle_plan, WebsiteExtendedState, WebsiteLifecyclePlan};
pub use server::WebServer;
pub use spec::{default_spec, ContainerPosture, HttpsSpec, SpecEndpoint, WebsiteSpec};

/// The integration's declaration, validated at first access.
#[doc(hidden)]
pub static MANIFEST: LazyLock<IntegrationManifest> = LazyLock::new(|| {
    IntegrationManifest::builtin(
        zim_engine_domain::WorkloadKind::Website,
        "org.zaminmc.workload.website",
        "0.1.0",
        "Website",
        "Static and dynamic web applications.",
        [
            Capability::HealthCheckCapable,
            Capability::FilesystemCapable,
            Capability::NetworkCapable,
        ]
        .into_iter()
        .collect(),
        // The metrics sample and one-shot exec stay optional in the
        // placement contract (a Remote placement may not exec), the
        // same posture the M16 skeleton declared.
        [Capability::MetricsCapable, Capability::ExecCapable]
            .into_iter()
            .collect(),
        &[RuntimeKindRef::Container, RuntimeKindRef::Remote],
    )
});

/// The Website integration's singleton.
pub struct WebsiteIntegration;

impl WorkloadIntegration for WebsiteIntegration {
    fn manifest(&self) -> &'static IntegrationManifest {
        &MANIFEST
    }

    /// Validate a workload row's spec against the Website schema.
    fn validate_spec(&self, spec: &Value) -> Result<(), SpecError> {
        let parsed: WebsiteSpec = serde_json::from_value(spec.clone())
            .map_err(|e| SpecError::Unparsable(e.to_string()))?;
        parsed.validate()
    }

    /// The kind's default spec: a static nginx site with the
    /// default health check (the API's creation form binds a
    /// server class and a domain at instantiation).
    fn default_spec(&self) -> Value {
        serde_json::to_value(default_spec()).expect("the default website spec is serializable")
    }

    /// The first-run install plan: the configuration write, the
    /// placeholder first release, and the current switch — as
    /// the ordered plan the install job family executes
    /// ([`build_install_plan`]).
    fn prepare_install(&self, spec: &Value) -> Result<Value, IntegrationError> {
        let parsed: WebsiteSpec = serde_json::from_value(spec.clone())
            .map_err(|e| SpecError::Unparsable(e.to_string()))?;
        parsed.validate()?;
        let plan = build_install_plan(&parsed);
        serde_json::to_value(&plan)
            .map_err(|e| IntegrationError::Spec(SpecError::Unparsable(e.to_string())))
    }

    /// The lifecycle translation: the signal-path power actions
    /// plus the deploy family's own actions.
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
    fn the_manifest_declares_the_website_contract() {
        let integration = WebsiteIntegration;
        assert_eq!(
            integration.kind(),
            zim_engine_domain::WorkloadKind::Website
        );
        let manifest = integration.manifest();
        assert_eq!(manifest.id, "org.zaminmc.workload.website");
        assert!(manifest.validate().is_ok());
        assert!(manifest.accepts_runtime(RuntimeKindRef::Container));
        assert!(manifest.accepts_runtime(RuntimeKindRef::Remote));
        for required in [
            Capability::HealthCheckCapable,
            Capability::FilesystemCapable,
            Capability::NetworkCapable,
        ] {
            assert!(manifest.required_capabilities.contains(required));
        }
        assert!(manifest
            .optional_capabilities
            .contains(Capability::MetricsCapable));
    }

    #[test]
    fn the_integration_validates_specs() {
        let integration = WebsiteIntegration;
        integration.validate_spec(&integration.default_spec()).unwrap();

        let bad = serde_json::json!({ "server": "apache" });
        assert!(integration.validate_spec(&bad).is_err());

        let malformed = serde_json::json!({
            "server": "static",
            "image": "nginx :bad"
        });
        assert!(integration.validate_spec(&malformed).is_err());
    }

    #[test]
    fn the_install_plan_is_the_documented_payload() {
        let integration = WebsiteIntegration;
        let plan = integration
            .prepare_install(&integration.default_spec())
            .unwrap();
        let steps: Vec<install::InstallStep> = serde_json::from_value(plan).unwrap();
        let names: Vec<&str> = steps.iter().map(|step| step.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["write-config", "first-release", "switch-current"]
        );
    }

    #[test]
    fn the_lifecycle_hooks_speak_the_website_vocabulary() {
        let integration = WebsiteIntegration;
        for action in ["start", "stop", "restart", "kill", "deploy", "drain"] {
            let plan = integration.lifecycle_plan(action).unwrap();
            let parsed: WebsiteLifecyclePlan = serde_json::from_value(plan).unwrap();
            assert_eq!(parsed.action, action);
        }
        let err = integration.lifecycle_plan("hibernate").unwrap_err();
        assert!(matches!(err, IntegrationError::Unsupported { .. }));
    }
}
