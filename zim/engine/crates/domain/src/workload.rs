//! Workload abstraction (expanded scope).
//!
//! # Source of truth
//!
//! Per `docs/architecture/WORKLOAD_ABSTRACTION.md` §3, a `Workload`
//! row references an `Instance` row (Pterodactyl-compatible base) plus
//! a `WorkloadKind` discriminator plus a typed `WorkloadSpec`.
//!
//! This M3 starter implements only the `WorkloadKind` enum — the
//! discriminator that the Instance struct will reference once it's
//! added in later M3 work. The full `Workload` struct and `WorkloadSpec`
//! enum (with `GameServerSpec`, `DatabaseSpec`, etc.) are added in
//! milestone M16 per the roadmap, when the workload integration crates
//! are scaffolded.

use serde::{Deserialize, Serialize};

/// The kind of workload an Instance represents.
///
/// Per `docs/architecture/WORKLOAD_ABSTRACTION.md` §3, the kind is
/// the discriminator that selects which workload integration handles
/// the workload's lifecycle, file operations, networking, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadKind {
    /// Minecraft and other game servers (the original Pterodactyl shape).
    ///
    /// This is the default kind per AD-17: the Pterodactyl migration
    /// importer auto-creates a `Workload` row of kind `GameServer` for
    /// each migrated `Instance` row.
    #[default]
    GameServer,
    /// Postgres, MySQL/MariaDB, Redis, MongoDB, etc. Requires
    /// `ReadyProbeCapable` and `SnapshotCapable` runtimes per the
    /// `database` first-party integration's declared capabilities.
    Database,
    /// Static and dynamic web apps. Requires `HealthCheckCapable` and
    /// `NetworkCapable` runtimes per the `website` integration.
    Website,
    /// Long-running dev processes (cargo watch, npm run dev, etc.).
    /// Requires `ConsoleCapable`; install/backup expectations are
    /// absent per the `dev_process` integration.
    DevProcess,
    /// Arbitrary containerized workloads. Capability negotiation
    /// happens at placement time per the `generic_container` integration.
    GenericContainer,
    /// Extension-provided workload kind (third-party integrations
    /// loaded via the integration package manifest per AD-15/AD-16).
    Custom,
}

impl WorkloadKind {
    /// Returns the canonical string name for this kind.
    ///
    /// Used in API responses, audit logs, and integration package
    /// manifests. The string form is the `snake_case` representation
    /// matching serde (e.g. `"game_server"`, `"dev_process"`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::GameServer => "game_server",
            Self::Database => "database",
            Self::Website => "website",
            Self::DevProcess => "dev_process",
            Self::GenericContainer => "generic_container",
            Self::Custom => "custom",
        }
    }

    /// Returns true if this kind is one of the five first-party
    /// workload integrations declared in
    /// `docs/architecture/COMPONENT_DISPOSITION.md` §7.
    pub fn is_first_party(&self) -> bool {
        !matches!(self, Self::Custom)
    }

    /// Returns the first-party integration crate name (under
    /// `zim/engine/crates/workloads/`) that implements this kind, or
    /// `None` for `Custom` (which is loaded from a third-party
    /// integration package per AD-15).
    pub fn integration_crate(&self) -> Option<&'static str> {
        match self {
            Self::GameServer => Some("zim_engine_workloads_minecraft"),
            Self::Database => Some("zim_engine_workloads_database"),
            Self::Website => Some("zim_engine_workloads_website"),
            Self::DevProcess => Some("zim_engine_workloads_dev_process"),
            Self::GenericContainer => Some("zim_engine_workloads_generic_container"),
            Self::Custom => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_game_server_for_pterodactyl_compat() {
        // Per AD-17: Pterodactyl migration importer auto-creates a
        // Workload of kind GameServer for each migrated Instance.
        assert_eq!(WorkloadKind::default(), WorkloadKind::GameServer);
    }

    #[test]
    fn kind_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&WorkloadKind::GameServer).unwrap(),
            "\"game_server\""
        );
        assert_eq!(
            serde_json::to_string(&WorkloadKind::DevProcess).unwrap(),
            "\"dev_process\""
        );
        assert_eq!(
            serde_json::to_string(&WorkloadKind::GenericContainer).unwrap(),
            "\"generic_container\""
        );
    }

    #[test]
    fn kind_round_trips_through_serde() {
        let k = WorkloadKind::Database;
        let s = serde_json::to_string(&k).unwrap();
        let k2: WorkloadKind = serde_json::from_str(&s).unwrap();
        assert_eq!(k, k2);
    }

    #[test]
    fn as_str_matches_serde_form() {
        // as_str() should return the same string that serde emits.
        for k in [
            WorkloadKind::GameServer,
            WorkloadKind::Database,
            WorkloadKind::Website,
            WorkloadKind::DevProcess,
            WorkloadKind::GenericContainer,
            WorkloadKind::Custom,
        ] {
            let serde_str = serde_json::to_string(&k).unwrap();
            let expected = format!("\"{}\"", k.as_str());
            assert_eq!(serde_str, expected, "mismatch for {:?}", k);
        }
    }

    #[test]
    fn first_party_kinds_are_known_set() {
        assert!(WorkloadKind::GameServer.is_first_party());
        assert!(WorkloadKind::Database.is_first_party());
        assert!(WorkloadKind::Website.is_first_party());
        assert!(WorkloadKind::DevProcess.is_first_party());
        assert!(WorkloadKind::GenericContainer.is_first_party());
        assert!(!WorkloadKind::Custom.is_first_party());
    }

    #[test]
    fn integration_crate_maps_to_workloads_subdirectory() {
        // Per docs/architecture/RUST_ENGINE.md §12.4, first-party
        // workload integrations live under
        // zim/engine/crates/workloads/{minecraft,database,website,
        // dev_process,generic_container}/. The integration_crate()
        // method returns the corresponding crate name.
        assert_eq!(
            WorkloadKind::GameServer.integration_crate(),
            Some("zim_engine_workloads_minecraft")
        );
        assert_eq!(
            WorkloadKind::Database.integration_crate(),
            Some("zim_engine_workloads_database")
        );
        assert_eq!(WorkloadKind::Custom.integration_crate(), None);
    }
}
