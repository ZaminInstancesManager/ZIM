//! Pterodactyl Panel DB importer (M10).
//!
//! Reads an existing Pterodactyl Panel database and produces a ZIM
//! engine SQLite database per the table mapping in
//! `docs/compatibility/MIGRATION_AND_COMPATIBILITY.md` §2.3:
//! users, servers → instances, allocations, nodes (RemoteLinux),
//! eggs reconstructed through the M9 legacy importer,
//! server_variables → instance_variables (migration 0004),
//! subusers → permission rows, schedules + tasks, backups and
//! transfers with their synthetic job companions (the FK
//! contract), activity_logs → audit. Deferred tables (nests,
//! locations folded, databases, mounts, api_keys, settings) are
//! counted and warned, never mapped.
//!
//! # Layout
//!
//! - [`dto`] — the Panel's row shapes (the only place the Panel's
//!   column vocabulary exists in the engine).
//! - [`source`] — the `PanelSource` port and the JSON fixture
//!   adapter.
//! - [`mysql`] — the live read-only MySQL adapter (feature
//!   `mysql`).
//! - [`mapping`] — the §2.3 mapping layer and the transactional
//!   write phase.
//! - [`report`] — the import report (rows imported / skipped with
//!   reasons / warnings) and its rendering.
//! - [`cli`] — the `zim migrate pterodactyl` subcommand.
//!
//! # Composition
//!
//! [`import_panel`] takes any [`PanelSource`] and any `Persistence`
//! sink. The CLI composes the SQLite adapter (file-backed for a
//! real import; a private in-memory store for the `--dry-run`
//! rehearsal, so a dry run is a complete import — validation,
//! counts, skips, warnings and all — that writes nothing to the
//! operator's database). The write path is one transaction per
//! table with foreign keys bracketed off, per §2.4's process.
//!
//! # Architecture boundary
//!
//! This crate's role, dependencies, and permitted callers are
//! documented in `docs/architecture/RUST_ENGINE.md` (and the
//! expanded-scope `docs/architecture/WORKLOAD_ABSTRACTION.md`
//! where applicable). The permitted dependency direction is
//! enforced at compile time by the workspace crate graph: the
//! importer sits at the tooling edge (like `zim_engine_main`) and
//! depends downward only — domain, application, eggs, and the
//! SQLite adapter it drives through the `Persistence` port.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cli;
pub mod dto;
pub mod mapping;
#[cfg(feature = "mysql")]
pub mod mysql;
pub mod report;
pub mod source;

pub use report::{DeferredTable, ImportReport, SkippedRow, TableOutcome};
pub use source::{JsonPanelSource, PanelSource, PanelSourceError, PanelTables};

use zim_engine_application::ports::Persistence;

/// The canonical crate name.
pub const CRATE_NAME: &str = "zim_engine_migrate_pterodactyl";

/// The security policy an import runs under — the two legacy
/// feature flags ZIM refuses without an explicit admin opt-in
/// (§3.4), passed straight through to the egg importer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImportPolicy {
    /// Allow eggs whose install scripts run privileged
    /// (`script_is_privileged` in the Panel's eggs table).
    pub allow_privileged_install_scripts: bool,
    /// Allow eggs whose workloads must run as root.
    pub allow_requires_root: bool,
}

/// Why an import could not complete. Row-level problems are *not*
/// here — they are skips with reasons in the [`ImportReport`];
/// only source failures and write-phase failures abort the import.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// The panel source could not be read.
    #[error("the panel source failed: {0}")]
    Source(#[from] PanelSourceError),
    /// The write phase failed; the failed table's transaction was
    /// rolled back (earlier tables' commits stand — re-run the
    /// import into a fresh database per §2.4's process).
    #[error("the import could not be written: {0}")]
    Persistence(#[from] zim_engine_application::ports::PersistenceError),
}

/// Import a whole panel database into the sink (see the crate
/// docs). The source is read once; the mapping phase resolves
/// cross-references and records every decision; the write phase
/// emits tables in FK-safe order, one transaction per table, with
/// foreign keys bracketed off for its duration.
///
/// The sink must already be migrated (the CLI does this; tests use
/// `SqlitePersistence::open_in_memory` + `migrate`). Re-running an
/// import of the same source converges on the same *entity* rows
/// (the store's upserts, plus the egg importer's legacy-UUID and
/// the backup's UUID deduplication); history rows — audit, and the
/// transfer/job companions — record each pass. §2.4's process
/// remains the recommendation: import into a fresh database.
pub fn import_panel(
    source: &dyn PanelSource,
    sink: &dyn Persistence,
    policy: &ImportPolicy,
) -> Result<ImportReport, ImportError> {
    let tables = source.load()?;
    let mapped = mapping::map_tables(&tables, sink, policy);
    mapping::write_all(sink, &mapped)?;
    Ok(mapped.report)
}

#[cfg(test)]
mod tests {
    use super::CRATE_NAME;

    #[test]
    fn crate_name_is_set() {
        assert!(!CRATE_NAME.is_empty());
        assert_eq!(CRATE_NAME, "zim_engine_migrate_pterodactyl");
    }
}
