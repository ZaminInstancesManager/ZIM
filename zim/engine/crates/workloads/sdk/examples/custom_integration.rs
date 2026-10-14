//! An example workload integration (the `sdk/examples/` acceptance
//! surface for M16): how to declare one with the macro, and how to
//! implement the trait by hand when an integration outgrows the
//! declaration-only form.
//!
//! Run with:
//!
//! ```text
//! cargo run -p zim_engine_workloads_sdk --example custom_integration
//! ```

use zim_engine_domain::WorkloadKind;
use zim_engine_workloads_sdk::workload_integration;
use zim_engine_workloads_sdk::{
    Capability, CapabilitySet, IntegrationError, IntegrationManifest, RuntimeKindRef, SpecError,
    WorkloadIntegration,
};

// ── Form 1: the declaration macro ────────────────────────────────
//
// The metadata-only form: the macro validates the vocabulary at
// compile time, generates the manifest, and implements the trait's
// metadata methods. The behavioral hooks keep their `Unsupported`
// defaults until the integration is ready to own them.

#[workload_integration(
    kind = GameServer,
    id = "org.example.workload.battleship",
    version = "0.1.0",
    display_name = "Battleship Server",
    description = "An example game-server integration.",
    required_capabilities = [ConsoleCapable, FilesystemCapable],
    optional_capabilities = [MetricsCapable],
    runtime_kinds = [Container],
)]
pub struct BattleshipServer;

// ── Form 2: a hand-written impl ─────────────────────────────────
//
// The same contract, implemented manually — the form an integration
// uses when it wants custom behavior, custom validation, or a spec
// schema of its own.

pub struct VesselServer;

impl WorkloadIntegration for VesselServer {
    fn manifest(&self) -> &'static IntegrationManifest {
        static MANIFEST: std::sync::LazyLock<IntegrationManifest> =
            std::sync::LazyLock::new(|| {
                IntegrationManifest::builtin(
                    WorkloadKind::GameServer,
                    "org.example.workload.vessel",
                    "0.2.0",
                    "Vessel Server",
                    "The hand-written form of the example.",
                    [Capability::ConsoleCapable].into_iter().collect(),
                    CapabilitySet::EMPTY,
                    &[RuntimeKindRef::Container],
                )
            });
        &MANIFEST
    }

    // A real spec check (the macro form leaves this defaulted).
    fn validate_spec(&self, spec: &serde_json::Value) -> Result<(), SpecError> {
        let fleet = spec
            .get("fleet_size")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| SpecError::Invalid {
                field: "fleet_size".to_owned(),
                message: "must be a non-negative integer".to_owned(),
            })?;
        if fleet == 0 || fleet > 100 {
            return Err(SpecError::Invalid {
                field: "fleet_size".to_owned(),
                message: "must be between 1 and 100 ships".to_owned(),
            });
        }
        Ok(())
    }
}

fn main() {
    // Both forms satisfy the same contract.
    let battleship = BattleshipServer;
    let vessel = VesselServer;

    println!("== declaration macro form ==");
    println!("id:        {}", battleship.id());
    println!("kind:      {:?}", battleship.kind());
    println!("requires:  {}", battleship.runtime_requirements());
    println!("optional:  {}", battleship.manifest().optional_capabilities);
    println!(
        "runtimes:  {:?}",
        battleship
            .manifest()
            .runtime_kinds
            .iter()
            .map(|k| k.as_str())
            .collect::<Vec<_>>()
    );

    println!();
    println!("== hand-written form ==");
    println!("id:        {}", vessel.id());

    // The hand-written form owns its spec validation.
    let good = serde_json::json!({ "fleet_size": 12 });
    let bad = serde_json::json!({ "fleet_size": 0 });
    println!(
        "spec {{fleet_size: 12}} → {:?}",
        vessel.validate_spec(&good)
    );
    println!("spec {{fleet_size: 0}}  → {:?}", vessel.validate_spec(&bad));

    // The macro form's behavioral hooks answer `Unsupported`.
    match battleship.prepare_install(&serde_json::Value::Null) {
        Err(IntegrationError::Unsupported {
            integration,
            operation,
        }) => {
            println!(
                "{}: {} is not implemented yet (the skeleton default)",
                integration, operation
            );
        }
        other => unreachable!("the macro form's default answers Unsupported, got {other:?}"),
    }
}
