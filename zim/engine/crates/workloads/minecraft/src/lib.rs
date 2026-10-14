//! The Minecraft Server workload integration (GameServer, M16 skeleton).
//!
//! Declared with `#[workload_integration]`; the behavioral surface
//! (spec schemas, lifecycle semantics) arrives with the kind's
//! implementation milestone.
//!
//! Understands (the M17 parity scope): EULA acceptance, server JAR
//! management, startup flags, game versions, player counts, and the
//! Minecraft console conventions (`/say`, `/stop`, `/list`, …) that
//! ride the runtime's console capability.
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

use zim_engine_workloads_sdk::workload_integration;

/// The Minecraft Server integration's singleton.
#[workload_integration(
    kind = GameServer,
    id = "org.zaminmc.workload.minecraft",
    version = "0.1.0",
    display_name = "Minecraft Server",
    description = "Minecraft and Minecraft-compatible game servers.",
    required_capabilities = [ConsoleCapable, FilesystemCapable, MetricsCapable, ExecCapable],
    optional_capabilities = [Migratable, SnapshotCapable],
    runtime_kinds = [Container, Remote],
)]
pub struct MinecraftIntegration;

#[cfg(test)]
mod tests {
    use super::*;
    use zim_engine_domain::WorkloadKind;
    use zim_engine_workloads_sdk::{Capability, RuntimeKindRef, WorkloadIntegration};

    #[test]
    fn declares_the_minecraft_contract() {
        assert_eq!(MinecraftIntegration.kind(), WorkloadKind::GameServer);
        let manifest = MinecraftIntegration.manifest();
        assert_eq!(manifest.id, "org.zaminmc.workload.minecraft");
        assert_eq!(manifest.display_name, "Minecraft Server");
        assert!(manifest.validate().is_ok());
        assert!(manifest.accepts_runtime(RuntimeKindRef::Container));
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
        // The behavioral hooks are still the SDK's defaults.
        assert!(MinecraftIntegration.runtime_requirements() == manifest.required_capabilities);
    }
}
