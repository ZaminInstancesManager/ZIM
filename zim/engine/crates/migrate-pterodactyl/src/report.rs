//! The import report: what landed, what was skipped and why, and
//! what was deliberately left behind.
//!
//! The report is the operator's contract with the importer per
//! `docs/compatibility/MIGRATION_AND_COMPATIBILITY.md` §2.4 step 4
//! ("rows imported, rows skipped (with reasons), and warnings") —
//! every non-fatal decision the importer makes is visible here,
//! and the CLI prints the whole thing.

use std::fmt;

/// One skipped source row, with the reason it did not land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedRow {
    /// Identifies the row in source terms (the Panel id, or the
    /// natural key when the row has none).
    pub key: String,
    /// Why the row was skipped.
    pub reason: String,
}

/// One table's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableOutcome {
    /// The ZIM table that was written (`users`, `instances`, ...).
    pub table: String,
    /// Rows that landed.
    pub imported: usize,
    /// Rows that did not, with reasons.
    pub skipped: Vec<SkippedRow>,
}

/// One deferred source table: counted, never mapped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredTable {
    /// The Panel table name.
    pub table: String,
    /// Rows counted in the source.
    pub rows: u64,
    /// Why it is deferred (the short operator-facing note).
    pub note: String,
}

/// The whole import's outcome (see the module docs).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// Per-table outcomes, in write order.
    pub tables: Vec<TableOutcome>,
    /// Deferred tables (counted + warned, not imported).
    pub deferred: Vec<DeferredTable>,
    /// Import-wide warnings (mapping folds, vocabulary notes,
    /// deferred-table notices).
    pub warnings: Vec<String>,
    /// True when the import was a rehearsal (dry-run): nothing was
    /// written to the operator's database.
    pub rehearsed: bool,
}

/// How many skipped-row detail lines each table prints before
/// collapsing into a "+N more" line (the CLI stays readable on
/// large panels; the full detail is always in the structure).
const SKIP_DETAIL_CAP: usize = 50;

impl ImportReport {
    /// Total rows imported across all tables.
    pub fn total_imported(&self) -> usize {
        self.tables.iter().map(|t| t.imported).sum()
    }

    /// Total rows skipped across all tables.
    pub fn total_skipped(&self) -> usize {
        self.tables.iter().map(|t| t.skipped.len()).sum()
    }

    /// Every table outcome for `table` (there is one per write
    /// phase; synthetic tables like `jobs` appear once).
    pub fn outcome(&self, table: &str) -> Option<&TableOutcome> {
        self.tables.iter().find(|t| t.table == table)
    }
}

impl fmt::Display for ImportReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Pterodactyl panel import report")?;
        writeln!(
            f,
            "  mode: {}",
            if self.rehearsed {
                "dry run (rehearsed against a scratch store; nothing written)"
            } else {
                "import (written)"
            }
        )?;
        for outcome in &self.tables {
            writeln!(
                f,
                "  {:<20} {:>6} imported, {:>3} skipped",
                outcome.table,
                outcome.imported,
                outcome.skipped.len()
            )?;
            for (index, skipped) in outcome.skipped.iter().enumerate() {
                if index >= SKIP_DETAIL_CAP {
                    writeln!(
                        f,
                        "      ... and {} more skipped rows",
                        outcome.skipped.len() - SKIP_DETAIL_CAP
                    )?;
                    break;
                }
                writeln!(f, "      skipped {}: {}", skipped.key, skipped.reason)?;
            }
        }
        if !self.deferred.is_empty() {
            writeln!(f, "  deferred source tables (counted, not imported):")?;
            for deferred in &self.deferred {
                writeln!(
                    f,
                    "    {:<18} {:>4} rows — {}",
                    deferred.table, deferred.rows, deferred.note
                )?;
            }
        }
        if !self.warnings.is_empty() {
            writeln!(f, "  warnings:")?;
            for warning in &self.warnings {
                writeln!(f, "    - {warning}")?;
            }
        }
        write!(
            f,
            "  total: {} row(s) imported, {} skipped, {} deferred table(s), {} warning(s)",
            self.total_imported(),
            self.total_skipped(),
            self.deferred.len(),
            self.warnings.len()
        )
    }
}
