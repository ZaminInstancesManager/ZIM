//! Port traits for outbound dependencies of the application layer.
//!
//! # Architectural boundary
//!
//! Per `docs/architecture/RUST_ENGINE.md` §2, the application layer
//! depends only on `zim_engine_domain` and declares its outbound
//! dependencies as *port traits*. Concrete adapters (SQLite, Docker,
//! ...) implement the traits and are injected at composition time by
//! `zim_engine_main`.
//!
//! # Sync now, async later (decision)
//!
//! The `Persistence` port is **synchronous** in M3. The engine's
//! concurrency-critical paths (supervision, streaming) go through
//! the `Runtime` port, not persistence; the SQLite adapter is
//! `Mutex<Connection>`-based and fast enough that a sync port keeps
//! M3 testable without an async runtime. Since M5 the HTTP API
//! bridges every call through `tokio::task::spawn_blocking` (the
//! trait is `Send + Sync` for exactly this purpose) — a
//! well-trodden pattern that avoids holding a runtime worker while
//! a DB lock is held. If profiling later shows contention, the port
//! can move to an async trait with a connection pool without
//! changing the domain layer.

use zim_engine_domain::{
    Allocation, AuditRecord, Backup, Egg, EggId, ExtensionRegistration, Instance, InstanceId,
    InstanceStatus, Job, JobId, JobState, Node, NodeId, Permission, Schedule, ScheduleId, Transfer,
    TransferId, User, UserId,
};

/// Errors returned by [`Persistence`] implementations.
#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    /// The underlying store failed (SQL error, I/O error, ...).
    #[error("storage backend error: {0}")]
    Backend(String),
    /// A entity failed to (de)serialize (corrupt JSON column, enum
    /// string not in the vocabulary, ...).
    #[error("serialization error: {0}")]
    Serialization(String),
    /// The store's schema version is newer than this binary knows
    /// how to handle (forward-only migrations; upgrade the engine).
    #[error("schema version {found} is newer than the maximum supported {supported}")]
    SchemaTooNew {
        /// The version found in the store.
        found: u64,
        /// The maximum version this binary supports.
        supported: u64,
    },
    /// A migration failed to apply; the transaction was rolled back.
    #[error("migration {version} ({name}) failed: {reason}")]
    MigrationFailed {
        /// The version of the failed migration.
        version: u64,
        /// The name of the failed migration.
        name: String,
        /// The failure reason.
        reason: String,
    },
}

impl PersistenceError {
    /// Shorthand constructor for [`PersistenceError::Backend`].
    pub fn backend(reason: impl Into<String>) -> Self {
        Self::Backend(reason.into())
    }

    /// Shorthand constructor for
    /// [`PersistenceError::Serialization`].
    pub fn serialization(reason: impl Into<String>) -> Self {
        Self::Serialization(reason.into())
    }
}

/// The persistence port: durable storage for all engine metadata.
///
/// Semantics:
///
/// - **Upsert semantics.** `put_*` inserts or replaces by primary
///   key. There is no separate update method; the application layer
///   reads, mutates, and puts back.
/// - **No NotFound errors.** `get_*` returns `Ok(None)` when the
///   entity does not exist; callers decide whether that is an error.
/// - **Append-only audit.** Audit records are never updated or
///   deleted; `append_audit` returns the assigned monotonic id.
/// - **Referential integrity.** Implementations enforce foreign keys
///   (nodes → instances → jobs/backups/transfers/schedules, users →
///   instances/permissions). Deleting a parent with children is a
///   [`PersistenceError::Backend`] (FK violation).
/// - **Forward-only migrations.** [`Persistence::migrate`] applies
///   pending migrations in order; there are no down-migrations per
///   `docs/architecture/RUST_ENGINE.md` §5.
pub trait Persistence: Send + Sync {
    // ── Schema lifecycle ────────────────────────────────────────────

    /// Apply all pending migrations, in order. Idempotent: applying
    /// an already-applied set is a no-op. Fails with
    /// [`PersistenceError::MigrationFailed`] (transaction rolled
    /// back) or [`PersistenceError::SchemaTooNew`].
    fn migrate(&self) -> Result<(), PersistenceError>;

    /// The currently applied schema version.
    fn schema_version(&self) -> Result<u64, PersistenceError>;

    // ── Instances ───────────────────────────────────────────────────

    /// Insert or replace an instance by id.
    fn put_instance(&self, instance: &Instance) -> Result<(), PersistenceError>;

    /// Fetch an instance by id (`Ok(None)` when absent).
    fn get_instance(&self, id: &InstanceId) -> Result<Option<Instance>, PersistenceError>;

    /// Delete an instance by id.
    fn delete_instance(&self, id: &InstanceId) -> Result<(), PersistenceError>;

    /// All instances placed on a node.
    fn list_instances_by_node(&self, node: &NodeId) -> Result<Vec<Instance>, PersistenceError>;

    /// All instances owned by a user.
    fn list_instances_by_owner(&self, owner: &UserId) -> Result<Vec<Instance>, PersistenceError>;

    /// Update only the desired status of an instance (hot path for
    /// lifecycle transitions).
    fn update_instance_status(
        &self,
        id: &InstanceId,
        status: InstanceStatus,
    ) -> Result<(), PersistenceError>;

    // ── Nodes ───────────────────────────────────────────────────────

    /// Insert or replace a node by id.
    fn put_node(&self, node: &Node) -> Result<(), PersistenceError>;

    /// Fetch a node by id.
    fn get_node(&self, id: &NodeId) -> Result<Option<Node>, PersistenceError>;

    /// Delete a node by id (fails if instances remain on it).
    fn delete_node(&self, id: &NodeId) -> Result<(), PersistenceError>;

    /// All nodes.
    fn list_nodes(&self) -> Result<Vec<Node>, PersistenceError>;

    // ── Allocations ─────────────────────────────────────────────────

    /// Insert or replace an allocation by id.
    fn put_allocation(&self, allocation: &Allocation) -> Result<(), PersistenceError>;

    /// Create a *new* free allocation on a node, letting the store
    /// assign the auto-increment id; returns the assigned id.
    ///
    /// `put_allocation` requires an explicit id (upsert semantics),
    /// which is wrong for fresh allocations — this method is the
    /// creation primitive the API's allocation endpoints use. The
    /// store enforces the `(node_id, ip, port)` uniqueness; a
    /// duplicate surfaces as a [`PersistenceError::Backend`].
    fn create_allocation(
        &self,
        node_id: &NodeId,
        ip: std::net::IpAddr,
        port: u16,
        note: Option<&str>,
    ) -> Result<zim_engine_domain::AllocationId, PersistenceError>;

    /// Fetch an allocation by id.
    fn get_allocation(
        &self,
        id: &zim_engine_domain::AllocationId,
    ) -> Result<Option<Allocation>, PersistenceError>;

    /// Delete an allocation by id.
    fn delete_allocation(
        &self,
        id: &zim_engine_domain::AllocationId,
    ) -> Result<(), PersistenceError>;

    /// All allocations on a node (assigned and free).
    fn list_allocations_by_node(&self, node: &NodeId) -> Result<Vec<Allocation>, PersistenceError>;

    /// The allocation assigned to a specific instance, if any.
    fn find_allocation_by_instance(
        &self,
        instance: &InstanceId,
    ) -> Result<Option<Allocation>, PersistenceError>;

    // ── Users ───────────────────────────────────────────────────────

    /// Insert or replace a user by id.
    fn put_user(&self, user: &User) -> Result<(), PersistenceError>;

    /// Fetch a user by id.
    fn get_user(&self, id: &UserId) -> Result<Option<User>, PersistenceError>;

    /// Fetch a user by (lowercase) email.
    fn get_user_by_email(&self, email: &str) -> Result<Option<User>, PersistenceError>;

    /// Delete a user by id (fails if owned instances remain).
    fn delete_user(&self, id: &UserId) -> Result<(), PersistenceError>;

    /// All users.
    fn list_users(&self) -> Result<Vec<User>, PersistenceError>;

    // ── Permissions ─────────────────────────────────────────────────

    /// Insert or replace an instance-scoped permission grant row.
    fn put_permission(&self, permission: &Permission) -> Result<(), PersistenceError>;

    /// Fetch the permission row for (user, instance).
    fn get_permission(
        &self,
        user: &UserId,
        instance: &InstanceId,
    ) -> Result<Option<Permission>, PersistenceError>;

    /// Delete the permission row for (user, instance).
    fn delete_permission(
        &self,
        user: &UserId,
        instance: &InstanceId,
    ) -> Result<(), PersistenceError>;

    /// All permission rows for an instance.
    fn list_permissions_by_instance(
        &self,
        instance: &InstanceId,
    ) -> Result<Vec<Permission>, PersistenceError>;

    /// All permission rows for a user.
    fn list_permissions_by_user(&self, user: &UserId) -> Result<Vec<Permission>, PersistenceError>;

    // ── Jobs ────────────────────────────────────────────────────────

    /// Insert or replace a job by id.
    fn put_job(&self, job: &Job) -> Result<(), PersistenceError>;

    /// Fetch a job by id.
    fn get_job(&self, id: &JobId) -> Result<Option<Job>, PersistenceError>;

    /// Delete a job by id (fails if linked backups/transfers remain).
    fn delete_job(&self, id: &JobId) -> Result<(), PersistenceError>;

    /// All jobs for an instance.
    fn list_jobs_by_instance(&self, instance: &InstanceId) -> Result<Vec<Job>, PersistenceError>;

    /// All jobs in a given state (e.g. `CleanupPending` for the
    /// background cleaner).
    fn list_jobs_by_state(&self, state: JobState) -> Result<Vec<Job>, PersistenceError>;

    /// Bump a job to a new state and touch `updated_at` (hot path
    /// for the job state machine).
    fn update_job_state(&self, id: &JobId, state: JobState) -> Result<(), PersistenceError>;

    // ── Backups ─────────────────────────────────────────────────────

    /// Insert or replace a backup by id.
    fn put_backup(&self, backup: &Backup) -> Result<(), PersistenceError>;

    /// Fetch a backup by id.
    fn get_backup(
        &self,
        id: &zim_engine_domain::BackupId,
    ) -> Result<Option<Backup>, PersistenceError>;

    /// Delete a backup by id.
    fn delete_backup(&self, id: &zim_engine_domain::BackupId) -> Result<(), PersistenceError>;

    /// All backups for an instance.
    fn list_backups_by_instance(
        &self,
        instance: &InstanceId,
    ) -> Result<Vec<Backup>, PersistenceError>;

    // ── Transfers ───────────────────────────────────────────────────

    /// Insert or replace a transfer by id.
    fn put_transfer(&self, transfer: &Transfer) -> Result<(), PersistenceError>;

    /// Fetch a transfer by id.
    fn get_transfer(&self, id: &TransferId) -> Result<Option<Transfer>, PersistenceError>;

    /// All transfers for an instance.
    fn list_transfers_by_instance(
        &self,
        instance: &InstanceId,
    ) -> Result<Vec<Transfer>, PersistenceError>;

    // ── Schedules ───────────────────────────────────────────────────

    /// Insert or replace a schedule by id.
    fn put_schedule(&self, schedule: &Schedule) -> Result<(), PersistenceError>;

    /// Fetch a schedule by id.
    fn get_schedule(&self, id: &ScheduleId) -> Result<Option<Schedule>, PersistenceError>;

    /// Delete a schedule by id.
    fn delete_schedule(&self, id: &ScheduleId) -> Result<(), PersistenceError>;

    /// All schedules for an instance.
    fn list_schedules_by_instance(
        &self,
        instance: &InstanceId,
    ) -> Result<Vec<Schedule>, PersistenceError>;

    /// Every schedule, across instances (the scheduler's scan).
    fn list_all_schedules(&self) -> Result<Vec<Schedule>, PersistenceError>;

    // ── Eggs ───────────────────────────────────────────────────────

    /// Insert or replace an egg (its variables are replaced
    /// wholesale).
    fn put_egg(&self, egg: &Egg) -> Result<(), PersistenceError>;

    /// Fetch an egg by id.
    fn get_egg(&self, id: &EggId) -> Result<Option<Egg>, PersistenceError>;

    /// Fetch an egg by its legacy Pterodactyl UUID (import
    /// deduplication).
    fn get_egg_by_legacy_uuid(&self, uuid: &str) -> Result<Option<Egg>, PersistenceError>;

    /// Every egg, ordered by name.
    fn list_eggs(&self) -> Result<Vec<Egg>, PersistenceError>;

    /// Delete an egg by id (refused while instances reference it;
    /// callers that want the friendly error pre-check with
    /// [`Persistence::list_instances_by_egg`]).
    fn delete_egg(&self, id: &EggId) -> Result<(), PersistenceError>;

    /// Every instance currently on an egg (the delete guard).
    fn list_instances_by_egg(&self, egg: &EggId) -> Result<Vec<Instance>, PersistenceError>;

    // ── Audit (append-only) ─────────────────────────────────────────

    /// Append an audit record; returns the assigned monotonic id.
    /// The record's `id` field is overwritten with the assigned id.
    fn append_audit(&self, record: &AuditRecord) -> Result<u64, PersistenceError>;

    /// Read audit records in id order, starting *after* `cursor`
    /// (use `None` for the beginning), at most `limit` records.
    fn list_audit(
        &self,
        cursor: Option<u64>,
        limit: usize,
    ) -> Result<Vec<AuditRecord>, PersistenceError>;

    // ── Extensions ──────────────────────────────────────────────────

    /// Insert or replace an extension registration by id.
    fn put_extension(&self, extension: &ExtensionRegistration) -> Result<(), PersistenceError>;

    /// Fetch an extension registration by id.
    fn get_extension(
        &self,
        id: &zim_engine_domain::ExtensionId,
    ) -> Result<Option<ExtensionRegistration>, PersistenceError>;

    /// Delete an extension registration by id.
    fn delete_extension(&self, id: &zim_engine_domain::ExtensionId)
        -> Result<(), PersistenceError>;

    /// All installed extensions.
    fn list_extensions(&self) -> Result<Vec<ExtensionRegistration>, PersistenceError>;
}
