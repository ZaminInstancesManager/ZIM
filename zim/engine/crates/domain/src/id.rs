//! Strongly-typed identifiers for ZIM domain entities.
//!
//! All ID types are wrappers around UUID v4 (or `u64` for legacy
//! Pterodactyl compatibility). Using newtypes prevents mixing up IDs
//! of different entity kinds at the type-system level: an
//! `InstanceId` cannot be passed where a `NodeId` is expected, even
//! though both wrap the same underlying `Uuid`.
//!
//! # Source of truth
//!
//! The ID type inventory mirrors `docs/architecture/RUST_ENGINE.md`
//! §3 (the `InstanceId`, `NodeId`, `UserId`, `AllocationId`, `EggId`,
//! `JobId`, `BackupId`, `TransferId`, `ScheduleId`, `ExtensionId`
//! newtypes) plus `WorkloadId` from `docs/architecture/
//! WORKLOAD_ABSTRACTION.md` §3.
//!
//! `AllocationId` is the only ID type that wraps `u64` rather than
//! `Uuid` — this matches the Pterodactyl Panel schema (verified in
//! `docs/audits/PTERODACTYL_ARCHITECTURE.md` §7.1.3, where the
//! `allocations` table uses an auto-incrementing integer primary
//! key) and is preserved for migration compatibility per AD-12.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Macro that declares a UUID-backed strongly-typed identifier.
///
/// The resulting type is transparent over `Uuid` for serde, implements
/// `Display`, and has a `new()` constructor that generates a fresh
/// random UUID v4.
macro_rules! uuid_id_type {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Generate a new random ID (UUID v4).
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Wrap an existing UUID.
            pub const fn from_uuid(u: Uuid) -> Self {
                Self(u)
            }

            /// Get the underlying UUID.
            pub const fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Display::fmt(&self.0, f)
            }
        }

        impl From<Uuid> for $name {
            fn from(u: Uuid) -> Self {
                Self(u)
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                s.parse::<Uuid>().map(Self)
            }
        }
    };
}

uuid_id_type!(
    InstanceId,
    "Identifier for an Instance (Pterodactyl-compatible workload base)."
);
uuid_id_type!(
    NodeId,
    "Identifier for a Node (local or remote execution host)."
);
uuid_id_type!(UserId, "Identifier for a User.");
uuid_id_type!(
    EggId,
    "Identifier for an Egg (Pterodactyl server definition)."
);
uuid_id_type!(
    EggVariableId,
    "Identifier for an EggVariable (one declared env var)."
);
uuid_id_type!(
    JobId,
    "Identifier for a durable Job (backup/transfer/install)."
);
uuid_id_type!(BackupId, "Identifier for a Backup.");
uuid_id_type!(TransferId, "Identifier for a Transfer.");
uuid_id_type!(ScheduleId, "Identifier for a Schedule.");
uuid_id_type!(ExtensionId, "Identifier for an installed Extension.");
uuid_id_type!(
    WorkloadId,
    "Identifier for a Workload (expanded scope per WORKLOAD_ABSTRACTION.md)."
);
uuid_id_type!(
    LocationId,
    "Identifier for a Location (node grouping: datacenter / region)."
);
uuid_id_type!(
    DatabaseHostId,
    "Identifier for a DatabaseHost (an external DB server connection record)."
);
uuid_id_type!(
    ManagedDatabaseId,
    "Identifier for a ManagedDatabase (one per-instance database row)."
);

/// Identifier for an allocation (port assignment on a node).
///
/// This is a `u64` rather than a UUID to match the Pterodactyl Panel
/// schema (verified in
/// `docs/audits/PTERODACTYL_ARCHITECTURE.md` §7.1.3, where the
/// `allocations` table uses an auto-incrementing integer primary key).
/// Preserved for migration compatibility per AD-12.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct AllocationId(pub u64);

impl AllocationId {
    /// Construct a new `AllocationId` from a `u64`.
    pub const fn new(id: u64) -> Self {
        Self(id)
    }
}

impl std::fmt::Display for AllocationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::str::FromStr for AllocationId {
    type Err = std::num::ParseIntError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<u64>().map(Self)
    }
}

#[cfg(test)]
mod tests {
    //! Round-trip and identity tests for the ID newtypes. These tests
    //! verify that:
    //! 1. Serde (de)serialization round-trips preserve the value.
    //! 2. Two distinct `new()` calls produce distinct IDs (the
    //!    underlying UUID generator is non-deterministic).
    //! 3. The newtype wrapper doesn't accidentally allow mixing ID
    //!    kinds at the type-system level — this is a compile-time
    //!    guarantee, but the tests document the intent.

    use super::*;

    #[test]
    fn instance_id_round_trips_through_serde() {
        let id = InstanceId::new();
        let s = serde_json::to_string(&id).unwrap();
        let id2: InstanceId = serde_json::from_str(&s).unwrap();
        assert_eq!(id, id2);
    }

    #[test]
    fn distinct_ids_are_distinct() {
        let a = InstanceId::new();
        let b = InstanceId::new();
        assert_ne!(a, b, "two new() calls should produce distinct IDs");
    }

    #[test]
    fn from_str_parses_valid_uuid() {
        let s = "550e8400-e29b-41d4-a716-446655440000";
        let id: InstanceId = s.parse().unwrap();
        assert_eq!(id.to_string(), s);
    }

    #[test]
    fn from_str_rejects_invalid_uuid() {
        let s = "not-a-uuid";
        let result: Result<InstanceId, _> = s.parse();
        assert!(result.is_err());
    }

    #[test]
    fn display_uses_canonical_uuid_format() {
        let id = InstanceId::from_uuid("550e8400-e29b-41d4-a716-446655440000".parse().unwrap());
        assert_eq!(id.to_string(), "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn uuid_conversion_round_trips() {
        let u: Uuid = "550e8400-e29b-41d4-a716-446655440000".parse().unwrap();
        let id = InstanceId::from(u);
        let u2: Uuid = id.into();
        assert_eq!(u, u2);
    }

    #[test]
    fn allocation_id_round_trips_through_serde() {
        let id = AllocationId::new(42);
        let s = serde_json::to_string(&id).unwrap();
        assert_eq!(s, "42");
        let id2: AllocationId = serde_json::from_str(&s).unwrap();
        assert_eq!(id, id2);
    }

    #[test]
    fn allocation_id_parses_u64_strings() {
        let id: AllocationId = "12345".parse().unwrap();
        assert_eq!(id, AllocationId::new(12345));
    }

    #[test]
    fn allocation_id_rejects_non_numeric() {
        let result: Result<AllocationId, _> = "not-a-number".parse();
        assert!(result.is_err());
    }

    // Compile-time check that the newtype pattern prevents mixing ID
    // kinds. The following code would NOT compile (and that is the
    // point):
    //
    // ```
    // let i = InstanceId::new();
    // let n = NodeId::new();
    // let _ = i == n; // type mismatch: expected InstanceId, found NodeId
    // ```
    //
    // We don't write the negative assertion as a test because the
    // Rust type system already enforces it at compile time; we just
    // document the intent here.
}
