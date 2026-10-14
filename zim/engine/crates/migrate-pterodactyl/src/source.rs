//! The panel source port: where the importer's rows come from.
//!
//! Two implementations:
//!
//! - [`JsonPanelSource`] — a single-file JSON fixture shaped exactly
//!   like [`PanelTables`] (table name → row array, Panel column
//!   names as keys). Used by tests, dry-run demos, and operators
//!   who dumped their panel with `mysqldump`-adjacent tooling.
//! - [`MysqlPanelSource`](crate::mysql::MysqlPanelSource) — the
//!   live read-only MySQL adapter (feature `mysql`).
//!
//! The port deliberately loads the whole panel into memory in one
//! [`PanelSource::load`] call: the mapping phase needs arbitrary
//! cross-references (servers → owners → users, variables → eggs →
//! servers), panels are small (thousands of rows, not millions),
//! and a single consistent snapshot beats streaming rows whose
//! siblings may change mid-import.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::dto::{
    PanelActivityLog, PanelActivitySubject, PanelAllocation, PanelBackup, PanelEgg,
    PanelEggVariable, PanelLocation, PanelNest, PanelNode, PanelSchedule, PanelScheduleTask,
    PanelServer, PanelServerTransfer, PanelServerVariable, PanelSubuser, PanelUser,
};

/// Why a panel source could not be read.
#[derive(Debug, thiserror::Error)]
pub enum PanelSourceError {
    /// The source could not be opened or queried (I/O, SQL, DSN).
    #[error("the panel source could not be read: {0}")]
    Read(String),
    /// A fixture parsed as JSON but did not match the panel shape.
    #[error("the panel fixture {path:?} is malformed: {reason}")]
    MalformedFixture {
        /// The fixture's path.
        path: String,
        /// What shape rule failed.
        reason: String,
    },
}

/// The whole Panel database, in memory (see the module docs for why
/// one snapshot rather than streaming).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PanelTables {
    /// `users` rows.
    pub(crate) users: Vec<PanelUser>,
    /// `servers` rows.
    pub(crate) servers: Vec<PanelServer>,
    /// `allocations` rows.
    pub(crate) allocations: Vec<PanelAllocation>,
    /// `nodes` rows.
    pub(crate) nodes: Vec<PanelNode>,
    /// `locations` rows (folded — counted and warned, not stored).
    pub(crate) locations: Vec<PanelLocation>,
    /// `nests` rows (deferred — counted and warned).
    pub(crate) nests: Vec<PanelNest>,
    /// `eggs` rows.
    pub(crate) eggs: Vec<PanelEgg>,
    /// `egg_variables` rows.
    pub(crate) egg_variables: Vec<PanelEggVariable>,
    /// `server_variables` rows.
    pub(crate) server_variables: Vec<PanelServerVariable>,
    /// `subusers` rows.
    pub(crate) subusers: Vec<PanelSubuser>,
    /// `schedules` rows.
    pub(crate) schedules: Vec<PanelSchedule>,
    /// `schedule_tasks` rows.
    pub(crate) schedule_tasks: Vec<PanelScheduleTask>,
    /// `backups` rows.
    pub(crate) backups: Vec<PanelBackup>,
    /// `activity_logs` rows.
    pub(crate) activity_logs: Vec<PanelActivityLog>,
    /// `activity_log_subjects` rows (the polymorphic server link).
    pub(crate) activity_log_subjects: Vec<PanelActivitySubject>,
    /// `server_transfers` rows.
    pub(crate) server_transfers: Vec<PanelServerTransfer>,
    /// Deferred tables (`databases`, `database_hosts`, `mounts`,
    /// `mount_node`, `mount_server`, `api_keys`, `settings`, ...)
    /// carried as raw row arrays purely so the report can count
    /// them — never mapped.
    #[serde(default)]
    pub(crate) deferred: BTreeMap<String, serde_json::Value>,
}

/// Where the importer's rows come from (see the module docs).
pub trait PanelSource {
    /// Read the whole panel database, read-only, into memory.
    fn load(&self) -> Result<PanelTables, PanelSourceError>;
}

/// A single-file JSON fixture source: the document's shape is
/// [`PanelTables`] — table name → row array, Panel column names as
/// row keys (see `tests/fixtures/` for a worked example).
pub struct JsonPanelSource {
    path: PathBuf,
}

impl JsonPanelSource {
    /// Wrap a fixture file path.
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl PanelSource for JsonPanelSource {
    fn load(&self) -> Result<PanelTables, PanelSourceError> {
        let text = std::fs::read_to_string(&self.path).map_err(|e| {
            PanelSourceError::Read(format!("cannot read fixture {}: {e}", self.path.display()))
        })?;
        let tables: PanelTables =
            serde_json::from_str(&text).map_err(|e| PanelSourceError::MalformedFixture {
                path: self.path.display().to_string(),
                reason: e.to_string(),
            })?;
        Ok(tables)
    }
}
