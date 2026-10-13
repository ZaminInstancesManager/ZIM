//! Canonical domain types for the ZIM engine.
//!
//! This crate contains the canonical domain types for the ZIM engine:
//! strongly-typed identifiers, lifecycle states, the permission
//! vocabulary (preserved verbatim from Pterodactyl per AD-12), and the
//! workload-kind discriminator introduced by the expanded scope.
//!
//! # Architectural boundary
//!
//! This crate MUST NOT depend on HTTP handlers, frontend packages,
//! Docker-specific types, or operating-system process APIs. This is
//! enforced at compile time by the permitted dependency direction in
//! `docs/architecture/RUST_ENGINE.md` §2 — any `use` of a forbidden
//! type is a compile error. The `[dependencies]` in this crate's
//! `Cargo.toml` therefore lists only `serde`, `serde_json`, `uuid`,
//! `chrono`, and `thiserror` (none of which carry I/O semantics that
//! would leak through the domain boundary).
//!
//! # Source of truth
//!
//! The type inventory here mirrors `docs/architecture/RUST_ENGINE.md`
//! §3 (domain model) and `docs/architecture/WORKLOAD_ABSTRACTION.md`
//! §3 (expanded-scope Workload model). Where the two documents
//! disagree about workload terminology, `WORKLOAD_ABSTRACTION.md`
//! is canonical per `docs/architecture/DECISIONS.md` AD-13.
//!
//! # Milestone scope (M3)
//!
//! This M3 starter implements:
//! - The 10 ID newtypes (`id` module).
//! - The lifecycle enums: `InstanceStatus`, `JobState`,
//!   `TransferState` (`status` module).
//! - The `PermissionGrant` enum (~50 grants) and `PermissionSet`
//!   (`permission` module).
//! - The `WorkloadKind` discriminator (`workload` module).
//!
//! The remaining domain types (Instance, Node, User, Job, Backup,
//! Transfer, Schedule, AuditRecord, ExtensionRegistration, plus the
//! expanded-scope Workload struct and WorkloadSpec enum) are added
//! later in M3 once their dependency on these foundational types is
//! verified.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod id;
pub mod permission;
pub mod status;
pub mod workload;

// Re-export the most commonly used types at the crate root for
// ergonomic access. Other modules should use the qualified path
// (`zim_engine_domain::id::InstanceId`) where ambiguity matters.
pub use id::{
    AllocationId, BackupId, EggId, ExtensionId, InstanceId, JobId, NodeId, ScheduleId, TransferId,
    UserId, WorkloadId,
};
pub use permission::{PermissionGrant, PermissionSet};
pub use status::{InstanceStatus, JobState, TransferState};
pub use workload::WorkloadKind;

/// The canonical crate name, useful for runtime identification in
/// logs and crash reports.
pub const CRATE_NAME: &str = "zim_engine_domain";

#[cfg(test)]
mod tests {
    //! Crate-level smoke tests that verify cross-module re-exports
    //! and basic type identity. Module-specific tests live alongside
    //! the types in their respective `*.rs` files.

    use super::*;

    #[test]
    fn crate_name_is_set() {
        assert_eq!(CRATE_NAME, "zim_engine_domain");
    }

    #[test]
    fn re_exports_are_visible_at_crate_root() {
        // Smoke check that the re-exports work. If any of these
        // fail to resolve, the `pub use` list in lib.rs is stale.
        let _id = InstanceId::new();
        let _node = NodeId::new();
        let _alloc = AllocationId::new(1);
        let _status = InstanceStatus::default();
        let _kind = WorkloadKind::default();
        let _grant = PermissionGrant::ControlStart;
        let _set = PermissionSet::new();
        let _job_state = JobState::Pending;
        let _transfer_state = TransferState::Pending;
        let _egg = EggId::new();
        let _job = JobId::new();
        let _backup = BackupId::new();
        let _transfer = TransferId::new();
        let _schedule = ScheduleId::new();
        let _extension = ExtensionId::new();
        let _user = UserId::new();
        let _workload = WorkloadId::new();
    }
}
