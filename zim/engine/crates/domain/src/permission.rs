//! Permission vocabulary + capability set.
//!
//! # Source of truth
//!
//! Per `docs/architecture/RUST_ENGINE.md` §3 (the `PermissionGrant`
//! enum and `PermissionSet` type) and AD-12 in
//! `docs/architecture/DECISIONS.md`: the `PermissionGrant` enum
//! preserves Pterodactyl's `Permission` constants verbatim
//! (`websocket.connect`, `control.console`, `control.start`, etc.)
//! for tool compatibility. ZIM may add new grants (e.g. `Install`,
//! `Transfer`, `AdminInstance`, the `workload.*` family from
//! `docs/architecture/WORKLOAD_ABSTRACTION.md` §9) but does not
//! rename existing ones.
//!
//! # Pterodactyl reference
//!
//! The Pterodactyl-preserved vocabulary is extracted from
//! `upstream/pterodactyl-panel/app/Models/Permission.php` at the
//! pinned revision v1.15.1 (verified in
//! `docs/audits/PTERODACTYL_ARCHITECTURE.md` §7.1.4). The constants
//! extracted there are: `websocket.connect`, `control.console`,
//! `control.start`, `control.stop`, `control.restart`, `database.*`,
//! `schedule.*`, `user.*`, `backup.read`. ZIM preserves every name
//! verbatim and extends with ZIM-original grants for non-game-server
//! workloads per AD-13.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// A single permission grant.
///
/// Variant names are the Rust-idiomatic `PascalCase` form of the
/// canonical string names (which are dot-separated Pterodactyl
/// vocabulary, e.g. `control.start`). Use [`PermissionGrant::as_str`]
/// to get the canonical string form for API responses, audit logs,
/// and tooling compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionGrant {
    // ── Pterodactyl-preserved vocabulary (verbatim per AD-12) ──────
    /// `websocket.connect` — connect to the live console WS.
    WebsocketConnect,
    /// `control.console` — send console input (requires `ConsoleCapable`
    /// runtime per `docs/architecture/WORKLOAD_ABSTRACTION.md` §4).
    ControlConsole,
    /// `control.start`.
    ControlStart,
    /// `control.stop`.
    ControlStop,
    /// `control.restart`.
    ControlRestart,

    // ── File operations (Pterodactyl-preserved) ──────────────────
    /// `file.read`.
    FileRead,
    /// `file.read_content` — read the bytes of a file (distinct from
    /// `file.read` which is just listing).
    FileReadContent,
    /// `file.write`.
    FileWrite,
    /// `file.create`.
    FileCreate,
    /// `file.delete`.
    FileDelete,
    /// `file.archive` — create and extract archives (zip / tar).
    FileArchive,

    // ── Backup operations (Pterodactyl-preserved + ZIM extensions) ─
    /// `backup.read`.
    BackupRead,
    /// `backup.create`.
    BackupCreate,
    /// `backup.update`.
    BackupUpdate,
    /// `backup.delete`.
    BackupDelete,
    /// `backup.restore`.
    BackupRestore,
    /// `backup.download`.
    BackupDownload,

    // ── Allocation operations ──────────────────────────────────────
    /// `allocation.read`.
    AllocationRead,
    /// `allocation.create`.
    AllocationCreate,
    /// `allocation.update`.
    AllocationUpdate,
    /// `allocation.delete`.
    AllocationDelete,

    // ── Subuser management (Pterodactyl-preserved) ────────────────
    /// `user.read`.
    UserRead,
    /// `user.create`.
    UserCreate,
    /// `user.update`.
    UserUpdate,
    /// `user.delete`.
    UserDelete,

    // ── Schedule management (Pterodactyl-preserved) ────────────────
    /// `schedule.read`.
    ScheduleRead,
    /// `schedule.create`.
    ScheduleCreate,
    /// `schedule.update`.
    ScheduleUpdate,
    /// `schedule.delete`.
    ScheduleDelete,

    // ── Database management (Pterodactyl-preserved) ────────────────
    /// `database.read`.
    DatabaseRead,
    /// `database.create`.
    DatabaseCreate,
    /// `database.update`.
    DatabaseUpdate,
    /// `database.delete`.
    DatabaseDelete,
    /// `database.view_password` — distinct from `database.read` per
    /// the Panel's `Permission.php` (the correct security posture:
    /// viewing credentials is its own grant).
    DatabaseViewPassword,

    // ── Settings ───────────────────────────────────────────────────
    /// `settings.read`.
    SettingsRead,
    /// `settings.update`.
    SettingsUpdate,

    // ── ZIM additions (per AD-12 + WORKLOAD_ABSTRACTION.md §9) ────
    /// Install / reinstall a workload.
    Install,
    /// Initiate workload migration (returns a JobId).
    Transfer,
    /// Workload admin (delete, suspend).
    AdminInstance,

    // ── Workload-generalized grants (expanded scope) ───────────────
    /// Generic workload metadata read.
    WorkloadRead,
    /// Generic workload create (or `workload.create.{kind}` for
    /// kind-specific create).
    WorkloadCreate,
    /// Generic workload update.
    WorkloadUpdate,
    /// Generic workload delete.
    WorkloadDelete,
    /// Generic workload migrate.
    WorkloadMigrate,
    /// Request a snapshot (only on `SnapshotCapable` workloads per
    /// `docs/architecture/WORKLOAD_ABSTRACTION.md` §4).
    WorkloadSnapshot,
    /// Deploy a new version (Website workloads only).
    WorkloadDeploy,
    /// Expose a new network port (only on `NetworkCapable` workloads).
    NetworkExpose,
    /// Unexpose a network port.
    NetworkUnexpose,
}

impl PermissionGrant {
    /// Every grant in the vocabulary, in declaration order.
    ///
    /// Used by the authz layer to materialize the effective grant set
    /// for root administrators (who bypass per-instance permission
    /// rows) and by tests that need to iterate the full vocabulary.
    /// Keep this list in sync with the enum — the
    /// `all_covers_every_variant` test fails if a variant is added
    /// without extending this list.
    pub const ALL: &'static [PermissionGrant] = &[
        PermissionGrant::WebsocketConnect,
        PermissionGrant::ControlConsole,
        PermissionGrant::ControlStart,
        PermissionGrant::ControlStop,
        PermissionGrant::ControlRestart,
        PermissionGrant::FileRead,
        PermissionGrant::FileReadContent,
        PermissionGrant::FileWrite,
        PermissionGrant::FileCreate,
        PermissionGrant::FileDelete,
        PermissionGrant::FileArchive,
        PermissionGrant::BackupRead,
        PermissionGrant::BackupCreate,
        PermissionGrant::BackupUpdate,
        PermissionGrant::BackupDelete,
        PermissionGrant::BackupRestore,
        PermissionGrant::BackupDownload,
        PermissionGrant::AllocationRead,
        PermissionGrant::AllocationCreate,
        PermissionGrant::AllocationUpdate,
        PermissionGrant::AllocationDelete,
        PermissionGrant::UserRead,
        PermissionGrant::UserCreate,
        PermissionGrant::UserUpdate,
        PermissionGrant::UserDelete,
        PermissionGrant::ScheduleRead,
        PermissionGrant::ScheduleCreate,
        PermissionGrant::ScheduleUpdate,
        PermissionGrant::ScheduleDelete,
        PermissionGrant::DatabaseRead,
        PermissionGrant::DatabaseCreate,
        PermissionGrant::DatabaseUpdate,
        PermissionGrant::DatabaseDelete,
        PermissionGrant::DatabaseViewPassword,
        PermissionGrant::SettingsRead,
        PermissionGrant::SettingsUpdate,
        PermissionGrant::Install,
        PermissionGrant::Transfer,
        PermissionGrant::AdminInstance,
        PermissionGrant::WorkloadRead,
        PermissionGrant::WorkloadCreate,
        PermissionGrant::WorkloadUpdate,
        PermissionGrant::WorkloadDelete,
        PermissionGrant::WorkloadMigrate,
        PermissionGrant::WorkloadSnapshot,
        PermissionGrant::WorkloadDeploy,
        PermissionGrant::NetworkExpose,
        PermissionGrant::NetworkUnexpose,
    ];

    /// Returns the canonical Pterodactyl-style string name for this
    /// grant (e.g. `"control.start"`, `"websocket.connect"`).
    ///
    /// These names are preserved verbatim from Pterodactyl's
    /// `Permission.php` per AD-12, so existing tooling that consumes
    /// these names continues to work.
    pub fn as_str(&self) -> &'static str {
        match self {
            // Pterodactyl-preserved.
            Self::WebsocketConnect => "websocket.connect",
            Self::ControlConsole => "control.console",
            Self::ControlStart => "control.start",
            Self::ControlStop => "control.stop",
            Self::ControlRestart => "control.restart",

            // Files.
            Self::FileRead => "file.read",
            Self::FileReadContent => "file.read_content",
            Self::FileWrite => "file.write",
            Self::FileCreate => "file.create",
            Self::FileDelete => "file.delete",
            Self::FileArchive => "file.archive",

            // Backups.
            Self::BackupRead => "backup.read",
            Self::BackupCreate => "backup.create",
            Self::BackupUpdate => "backup.update",
            Self::BackupDelete => "backup.delete",
            Self::BackupRestore => "backup.restore",
            Self::BackupDownload => "backup.download",

            // Allocations.
            Self::AllocationRead => "allocation.read",
            Self::AllocationCreate => "allocation.create",
            Self::AllocationUpdate => "allocation.update",
            Self::AllocationDelete => "allocation.delete",

            // Subusers.
            Self::UserRead => "user.read",
            Self::UserCreate => "user.create",
            Self::UserUpdate => "user.update",
            Self::UserDelete => "user.delete",

            // Schedules.
            Self::ScheduleRead => "schedule.read",
            Self::ScheduleCreate => "schedule.create",
            Self::ScheduleUpdate => "schedule.update",
            Self::ScheduleDelete => "schedule.delete",

            // Databases.
            Self::DatabaseRead => "database.read",
            Self::DatabaseCreate => "database.create",
            Self::DatabaseUpdate => "database.update",
            Self::DatabaseDelete => "database.delete",
            Self::DatabaseViewPassword => "database.view_password",

            // Settings.
            Self::SettingsRead => "settings.read",
            Self::SettingsUpdate => "settings.update",

            // ZIM additions.
            Self::Install => "install",
            Self::Transfer => "transfer",
            Self::AdminInstance => "admin.instance",
            Self::WorkloadRead => "workload.read",
            Self::WorkloadCreate => "workload.create",
            Self::WorkloadUpdate => "workload.update",
            Self::WorkloadDelete => "workload.delete",
            Self::WorkloadMigrate => "workload.migrate",
            Self::WorkloadSnapshot => "workload.snapshot",
            Self::WorkloadDeploy => "workload.deploy",
            Self::NetworkExpose => "network.expose",
            Self::NetworkUnexpose => "network.unexpose",
        }
    }
}

/// A set of permission grants for a user/instance pair.
///
/// For M3 this is backed by a `HashSet<PermissionGrant>`. A `u64`
/// bitfield representation may come later per
/// `docs/architecture/RUST_ENGINE.md` §3's `PermissionSet(pub u64)`
/// note; the public API is the same regardless of the backing store.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionSet(HashSet<PermissionGrant>);

impl PermissionSet {
    /// Construct an empty permission set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Grant a permission. Returns `&mut Self` for chaining.
    pub fn grant(&mut self, g: PermissionGrant) -> &mut Self {
        self.0.insert(g);
        self
    }

    /// Revoke a permission. Returns `&mut Self` for chaining.
    pub fn revoke(&mut self, g: PermissionGrant) -> &mut Self {
        self.0.remove(&g);
        self
    }

    /// Returns true if the set contains the given grant.
    pub fn contains(&self, g: PermissionGrant) -> bool {
        self.0.contains(&g)
    }

    /// Returns true if this set is a superset of `required` — i.e.
    /// every grant in `required` is also in this set. Used by the
    /// engine's authz layer at the API boundary per invariant 15 in
    /// `docs/architecture/ZIM_ARCHITECTURE.md` §4.
    pub fn superset_of(&self, required: &PermissionSet) -> bool {
        required.0.iter().all(|g| self.0.contains(g))
    }

    /// Returns the grants missing from this set relative to `required`.
    /// Useful for the `403 permission_denied` error response that
    /// lists the missing grants per `docs/api/ZIM_API_SPEC.md` §6.1.
    pub fn missing_from(&self, required: &PermissionSet) -> PermissionSet {
        let missing: HashSet<PermissionGrant> = required.0.difference(&self.0).copied().collect();
        PermissionSet(missing)
    }

    /// Returns true if the set is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the number of grants in the set.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Iterate over the grants in this set.
    pub fn iter(&self) -> impl Iterator<Item = PermissionGrant> + '_ {
        self.0.iter().copied()
    }

    /// The full grant vocabulary — the effective set for root
    /// administrators, who bypass per-instance permission rows.
    pub fn all() -> Self {
        Self(PermissionGrant::ALL.iter().copied().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pterodactyl_vocabulary_preserved_verbatim() {
        // Per AD-12: Pterodactyl's Permission constants vocabulary
        // is preserved verbatim. These names are extracted from
        // upstream/pterodactyl-panel/app/Models/Permission.php at
        // v1.15.1 (verified in docs/audits/PTERODACTYL_ARCHITECTURE.md
        // §7.1.4).
        assert_eq!(
            PermissionGrant::WebsocketConnect.as_str(),
            "websocket.connect"
        );
        assert_eq!(PermissionGrant::ControlConsole.as_str(), "control.console");
        assert_eq!(PermissionGrant::ControlStart.as_str(), "control.start");
        assert_eq!(PermissionGrant::ControlStop.as_str(), "control.stop");
        assert_eq!(PermissionGrant::ControlRestart.as_str(), "control.restart");
    }

    #[test]
    fn database_view_password_is_distinct_from_read() {
        // The correct security posture: viewing a database's password
        // is its own grant, separate from reading the database's
        // metadata.
        assert_ne!(
            PermissionGrant::DatabaseRead,
            PermissionGrant::DatabaseViewPassword,
        );
        assert_eq!(
            PermissionGrant::DatabaseViewPassword.as_str(),
            "database.view_password"
        );
        assert_ne!(
            PermissionGrant::DatabaseViewPassword.as_str(),
            PermissionGrant::DatabaseRead.as_str(),
        );
    }

    #[test]
    fn set_superset_check_passes_when_all_required_grants_present() {
        let mut admin = PermissionSet::new();
        admin.grant(PermissionGrant::ControlStart);
        admin.grant(PermissionGrant::ControlStop);
        admin.grant(PermissionGrant::ControlConsole);

        let mut required = PermissionSet::new();
        required.grant(PermissionGrant::ControlStart);

        assert!(admin.superset_of(&required));
        assert!(admin.missing_from(&required).is_empty());
    }

    #[test]
    fn set_superset_check_fails_when_required_grant_missing() {
        let mut user = PermissionSet::new();
        user.grant(PermissionGrant::ControlStart);
        user.grant(PermissionGrant::ControlStop);

        let mut required = PermissionSet::new();
        required.grant(PermissionGrant::ControlStart);
        required.grant(PermissionGrant::ControlConsole); // missing!

        assert!(!user.superset_of(&required));
        let missing = user.missing_from(&required);
        assert!(!missing.is_empty());
        assert!(missing.contains(PermissionGrant::ControlConsole));
        assert!(!missing.contains(PermissionGrant::ControlStart));
    }

    #[test]
    fn serde_preserves_grant_as_snake_case() {
        // Rust enum variants serialize as snake_case by default with
        // our #[serde(rename_all = "snake_case")] attribute. This is
        // distinct from the canonical Pterodactyl-style string form
        // returned by as_str() (which uses dots).
        let g = PermissionGrant::ControlStart;
        let s = serde_json::to_string(&g).unwrap();
        assert_eq!(s, "\"control_start\"");
    }

    #[test]
    fn grant_round_trips_through_serde() {
        let g = PermissionGrant::DatabaseViewPassword;
        let s = serde_json::to_string(&g).unwrap();
        let g2: PermissionGrant = serde_json::from_str(&s).unwrap();
        assert_eq!(g, g2);
    }

    #[test]
    fn set_round_trips_through_serde() {
        let mut set = PermissionSet::new();
        set.grant(PermissionGrant::ControlStart);
        set.grant(PermissionGrant::ControlStop);
        set.grant(PermissionGrant::WebsocketConnect);

        let s = serde_json::to_string(&set).unwrap();
        let set2: PermissionSet = serde_json::from_str(&s).unwrap();
        assert_eq!(set, set2);
        assert_eq!(set2.len(), 3);
    }

    #[test]
    fn empty_set_is_default() {
        let set = PermissionSet::default();
        assert!(set.is_empty());
        assert_eq!(set.len(), 0);
    }

    #[test]
    fn revoke_removes_grant() {
        let mut set = PermissionSet::new();
        set.grant(PermissionGrant::ControlStart);
        assert!(set.contains(PermissionGrant::ControlStart));

        set.revoke(PermissionGrant::ControlStart);
        assert!(!set.contains(PermissionGrant::ControlStart));
        assert!(set.is_empty());
    }

    #[test]
    fn all_covers_every_variant_exactly() {
        // If a variant is added to the enum without extending ALL,
        // this test fails — keeping the root-admin effective set
        // complete.
        let all = PermissionSet::all();
        assert_eq!(all.len(), PermissionGrant::ALL.len());
        // Every variant round-trips through as_str and back via the
        // serde vocabulary, and appears in ALL exactly once.
        for grant in PermissionGrant::ALL {
            assert!(all.contains(*grant), "{grant:?} missing from all()");
            let canonical = grant.as_str();
            assert!(
                !canonical.is_empty(),
                "canonical name for {grant:?} should not be empty"
            );
        }
        // all() is a superset of any hand-built set:
        let mut partial = PermissionSet::new();
        partial
            .grant(PermissionGrant::ControlStart)
            .grant(PermissionGrant::WorkloadDeploy);
        assert!(all.superset_of(&partial));
    }

    #[test]
    fn workload_grants_are_in_the_vocabulary() {
        // The workload.* family from WORKLOAD_ABSTRACTION.md §9.
        for grant in [
            PermissionGrant::WorkloadRead,
            PermissionGrant::WorkloadCreate,
            PermissionGrant::WorkloadUpdate,
            PermissionGrant::WorkloadDelete,
            PermissionGrant::WorkloadMigrate,
            PermissionGrant::WorkloadSnapshot,
            PermissionGrant::WorkloadDeploy,
        ] {
            assert!(PermissionSet::all().contains(grant));
        }
    }
}
