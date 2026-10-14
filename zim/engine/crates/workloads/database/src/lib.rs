//! The Database workload integration (M18).
//!
//! Declared per the §5 contract (identity, kind, capabilities,
//! runtime classes) in the hand-written form — the SDK example's
//! "Form 2", the shape an integration takes when it outgrows the
//! declaration-only macro form (the macro generates the trait impl
//! with `Unsupported` hooks; M18 owns the hooks, so the impl is
//! hand-written against the same generated-manifest shape).
//!
//! The M18 behavioral surface — the roadmap's §M18 parity:
//!
//! - [`DatabaseSpec`] parsing (engine table + user decisions:
//!   image override, credentials, endpoints, container posture);
//! - [`prepare_install`](DatabaseIntegration::prepare_install) —
//!   the first-boot initialization plan (env placement, the
//!   engine's initdb / mariadb-install-db / empty-RDB step, the
//!   readiness wait, the verification query);
//! - [`lifecycle_plan`](DatabaseIntegration::lifecycle_plan) — the
//!   database stop translation (the signal path: every M18
//!   engine's entrypoint turns SIGTERM into its orderly shutdown;
//!   there is no console) and the start/restart/kill translations;
//! - [`migration_steps`] — the §M18 multi-step migration plan
//!   (quiesce, checkpoint WAL, snapshot, ship snapshot, restore,
//!   catch up), the documented payload;
//! - [`DatabaseWorkload`] — the per-workload §4 capability handle
//!   (readiness probe, filesystem, snapshot, network, metrics,
//!   migration) over a live runtime.
//!
//! ```text
//! id      = org.zaminmc.workload.database
//! kind    = Database
//! requires: ReadyProbeCapable, FilesystemCapable, SnapshotCapable, NetworkCapable, MetricsCapable
//! optional: Migratable
//! runtimes: Container, Remote
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod engine;
pub mod handle;
pub mod install;
pub mod lifecycle;
pub mod probe;
pub mod spec;

use serde_json::Value;
use std::sync::LazyLock;

use zim_engine_workloads_sdk::errors::{IntegrationError, SpecError};
use zim_engine_workloads_sdk::{
    Capability, IntegrationManifest, RuntimeKindRef, WorkloadIntegration,
};

pub use engine::{DatabaseEngine, SnapshotMode};
pub use handle::DatabaseWorkload;
pub use install::{build_install_plan, DatabaseInstallPlan, InstallStep};
pub use lifecycle::{lifecycle_plan, migration_steps, DatabaseLifecyclePlan, MigrationStep};
pub use probe::{classify_probe, ready_line};
pub use spec::{default_spec, DatabaseCredentials, DatabaseSpec, SpecEndpoint};

/// The integration's declaration, validated at first access.
#[doc(hidden)]
pub static MANIFEST: LazyLock<IntegrationManifest> = LazyLock::new(|| {
    IntegrationManifest::builtin(
        zim_engine_domain::WorkloadKind::Database,
        "org.zaminmc.workload.database",
        "0.1.0",
        "Database",
        "Managed databases: Postgres, MySQL/MariaDB, Redis.",
        [
            Capability::ReadyProbeCapable,
            Capability::FilesystemCapable,
            Capability::SnapshotCapable,
            Capability::NetworkCapable,
            Capability::MetricsCapable,
        ]
        .into_iter()
        .collect(),
        // Migratable stays optional in the placement contract (a
        // Remote placement may not migrate), even though M18's
        // handle implements it — the same posture the M16 skeleton
        // declared.
        [Capability::Migratable].into_iter().collect(),
        &[RuntimeKindRef::Container, RuntimeKindRef::Remote],
    )
});

/// The Database integration's singleton.
pub struct DatabaseIntegration;

impl WorkloadIntegration for DatabaseIntegration {
    fn manifest(&self) -> &'static IntegrationManifest {
        &MANIFEST
    }

    /// Validate a workload row's spec against the Database schema.
    fn validate_spec(&self, spec: &Value) -> Result<(), SpecError> {
        let parsed: DatabaseSpec = serde_json::from_value(spec.clone())
            .map_err(|e| SpecError::Unparsable(e.to_string()))?;
        parsed.validate()
    }

    /// The kind's default spec: a Postgres workload with the
    /// engine's table defaults and a clearly-marked development
    /// password (the API's creation form binds real credentials at
    /// instantiation).
    fn default_spec(&self) -> Value {
        serde_json::to_value(default_spec()).expect("the default database spec is serializable")
    }

    /// The first-run install plan: the env placement, the image's
    /// own initialization step, the readiness wait, and the
    /// verification query — as the ordered plan the install job
    /// family executes.
    fn prepare_install(&self, spec: &Value) -> Result<Value, IntegrationError> {
        let parsed: DatabaseSpec = serde_json::from_value(spec.clone())
            .map_err(|e| SpecError::Unparsable(e.to_string()))?;
        parsed.validate()?;
        let plan = build_install_plan(&parsed);
        serde_json::to_value(&plan)
            .map_err(|e| IntegrationError::Spec(SpecError::Unparsable(e.to_string())))
    }

    /// The lifecycle translation for one action (`start`, `stop`,
    /// `restart`, `kill`); unknown actions answer `Unsupported`.
    ///
    /// The spec-less call answers the Postgres-defaulted form (the
    /// API's "what would stop do here" preview); spec-carrying
    /// callers use [`lifecycle_plan`] directly.
    fn lifecycle_plan(&self, action: &str) -> Result<Value, IntegrationError> {
        let plan = lifecycle_plan(&default_spec(), action)?;
        serde_json::to_value(&plan)
            .map_err(|e| IntegrationError::Spec(SpecError::Unparsable(e.to_string())))
    }
}

/// The canonical crate name.
pub const CRATE_NAME: &str = "zim_engine_workloads_database";

#[cfg(test)]
mod tests {
    use super::*;
    use zim_engine_domain::WorkloadKind;
    use zim_engine_workloads_sdk::{Capability, RuntimeKindRef};

    #[test]
    fn declares_the_database_contract() {
        assert_eq!(DatabaseIntegration.kind(), WorkloadKind::Database);
        let manifest = DatabaseIntegration.manifest();
        assert_eq!(manifest.id, "org.zaminmc.workload.database");
        assert_eq!(manifest.display_name, "Database");
        assert!(manifest.validate().is_ok());
        assert!(manifest.accepts_runtime(RuntimeKindRef::Container));
        assert!(manifest.accepts_runtime(RuntimeKindRef::Remote));
        for capability in [
            Capability::ReadyProbeCapable,
            Capability::FilesystemCapable,
            Capability::SnapshotCapable,
            Capability::NetworkCapable,
            Capability::MetricsCapable,
        ] {
            assert!(manifest.required_capabilities.contains(capability));
        }
        assert!(manifest
            .optional_capabilities
            .contains(Capability::Migratable));
        assert_eq!(
            DatabaseIntegration.runtime_requirements(),
            manifest.required_capabilities
        );
    }

    #[test]
    fn the_behavioral_hooks_answer_real_values() {
        // M18's contract: the hooks answer real values, not the
        // M16 defaults.
        let default_spec = DatabaseIntegration.default_spec();
        DatabaseIntegration
            .validate_spec(&default_spec)
            .expect("the default spec validates");

        let plan_value = DatabaseIntegration
            .prepare_install(&default_spec)
            .expect("prepare_install is implemented");
        let plan: DatabaseInstallPlan = serde_json::from_value(plan_value).unwrap();
        assert!(plan
            .steps
            .iter()
            .any(|step| matches!(step, InstallStep::InitializeDataDirectory { .. })));

        let lifecycle = DatabaseIntegration
            .lifecycle_plan("stop")
            .expect("lifecycle_plan is implemented");
        let plan: DatabaseLifecyclePlan = serde_json::from_value(lifecycle).unwrap();
        assert!(plan.stop_semantics.contains("SIGTERM"));
    }

    #[test]
    fn a_garbage_spec_is_refused_at_validation() {
        assert!(DatabaseIntegration.validate_spec(&Value::Null).is_err());
        assert!(DatabaseIntegration
            .validate_spec(&serde_json::json!({ "engine": "postgres" }))
            .is_err());
        let err = DatabaseIntegration
            .prepare_install(&Value::Null)
            .unwrap_err();
        assert!(err.to_string().contains("spec"), "{err}");
        let err = DatabaseIntegration.lifecycle_plan("hibernate").unwrap_err();
        assert!(matches!(err, IntegrationError::Unsupported { .. }));
    }

    #[test]
    fn the_default_spec_round_trips_into_the_typed_form() {
        let value = DatabaseIntegration.default_spec();
        let spec: DatabaseSpec = serde_json::from_value(value).unwrap();
        spec.validate().unwrap();
        assert_eq!(spec.engine, DatabaseEngine::Postgres);
        assert_eq!(spec.effective_image(), "postgres:16-alpine");
        assert_eq!(spec.primary_container_port(), 5432);
    }

    #[test]
    fn crate_name_is_set() {
        assert_eq!(CRATE_NAME, "zim_engine_workloads_database");
    }
}
