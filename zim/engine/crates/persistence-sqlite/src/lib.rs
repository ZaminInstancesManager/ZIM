//! SQLite persistence adapter.
//!
//! Implements the persistence port trait declared in
//! `zim_engine_application::ports::Persistence` using SQLite (WAL
//! mode). This is the default persistence adapter for single-machine
//! ZIM installations per `docs/architecture/RUST_ENGINE.md` §5.
//!
//! # Architecture boundary
//!
//! This crate's role, dependencies, and permitted callers are documented
//! in `docs/architecture/RUST_ENGINE.md` (and the expanded-scope
//! `docs/architecture/WORKLOAD_ABSTRACTION.md` where applicable).
//! The permitted dependency direction is enforced at compile time by
//! the workspace crate graph.
//!
//! # Storage conventions
//!
//! (Also documented in `migrations/0001_initial_schema.sql`.)
//!
//! - UUID identifiers are TEXT columns in the canonical hyphenated
//!   form; `AllocationId` is INTEGER (Pterodactyl auto-increment
//!   compatibility per AD-12).
//! - Timestamps are TEXT in RFC 3339, always UTC.
//! - Booleans are INTEGER 0/1.
//! - Enums are TEXT in their serde `snake_case` vocabulary; the
//!   schema's CHECK constraints and the enum serializers share that
//!   vocabulary as the single source of truth.
//! - Structured values (`ResourceLimits`, `NodeCapabilities`,
//!   permission grant lists, schedule actions, audit metadata,
//!   extension manifests and signatures) are TEXT JSON columns.
//! - Upserts use `INSERT ... ON CONFLICT DO UPDATE` (never
//!   `INSERT OR REPLACE`, which deletes-and-reinserts and would trip
//!   foreign keys on parent rows).
//!
//! # Concurrency model
//!
//! A single `rusqlite::Connection` is held behind a `Mutex`. The
//! engine is the only writer (per §5), the critical sections are
//! short single statements, and the async API layer (M5) will call
//! through `spawn_blocking`. WAL mode lets any future read-side
//! concurrency (reporting, the migration importer) open separate
//! read connections without blocking the writer.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod migrations;
mod store;

pub use migrations::MIGRATIONS;

/// SQLite-backed [`Persistence`] implementation.
///
/// Construct with [`SqlitePersistence::open`] (file-backed, WAL) or
/// [`SqlitePersistence::open_in_memory`] (tests, scratch stores).
/// Both start unmigrated: call [`Persistence::migrate`] before use.
pub struct SqlitePersistence {
    conn: std::sync::Mutex<rusqlite::Connection>,
}

impl SqlitePersistence {
    /// Open (or create) a file-backed store at `path`, with WAL
    /// journaling, NORMAL synchronous mode, foreign keys ON, and a
    /// 5 s busy timeout.
    pub fn open(path: impl AsRef<std::path::Path>) -> rusqlite::Result<Self> {
        let conn = rusqlite::Connection::open(path)?;
        Self::configure(&conn)?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
    }

    /// Open a private in-memory store (each call yields an
    /// independent database). Foreign keys ON. WAL is not applicable
    /// to memory databases (it is a no-op there).
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        let conn = rusqlite::Connection::open_in_memory()?;
        Self::configure(&conn)?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
    }

    fn configure(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
        conn.pragma_update(None, "journal_mode", "wal")?;
        conn.pragma_update(None, "synchronous", "normal")?;
        conn.pragma_update(None, "foreign_keys", "on")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(())
    }

    /// Run `f` with the guarded connection, mapping rusqlite errors
    /// into [`zim_engine_application::ports::PersistenceError`].
    pub(crate) fn with_conn<T>(
        &self,
        f: impl FnOnce(
            &rusqlite::Connection,
        ) -> Result<T, zim_engine_application::ports::PersistenceError>,
    ) -> Result<T, zim_engine_application::ports::PersistenceError> {
        let conn = self.conn.lock().map_err(|poisoned| {
            zim_engine_application::ports::PersistenceError::backend(format!(
                "connection mutex poisoned: {poisoned}"
            ))
        })?;
        f(&conn)
    }
}

#[cfg(test)]
mod tests {
    //! Adapter-level unit tests (connection setup, migration runner
    //! semantics). Full entity round-trip coverage lives in
    //! `tests/roundtrip.rs`.

    use super::*;

    #[test]
    fn open_in_memory_creates_usable_store() {
        let store = SqlitePersistence::open_in_memory().unwrap();
        let two: i64 = store
            .with_conn(|c| {
                c.query_row("SELECT 1 + 1", [], |row| row.get(0))
                    .map_err(|e| {
                        zim_engine_application::ports::PersistenceError::backend(e.to_string())
                    })
            })
            .unwrap();
        assert_eq!(two, 2);
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let store = SqlitePersistence::open_in_memory().unwrap();
        let fk_on: i64 = store
            .with_conn(|c| {
                c.query_row("PRAGMA foreign_keys", [], |row| row.get(0))
                    .map_err(|e| {
                        zim_engine_application::ports::PersistenceError::backend(e.to_string())
                    })
            })
            .unwrap();
        assert_eq!(fk_on, 1);
    }

    #[test]
    fn migration_register_is_nonempty_and_ordered() {
        assert!(!MIGRATIONS.is_empty());
        for pair in MIGRATIONS.windows(2) {
            assert!(
                pair[0].version < pair[1].version,
                "migrations must be strictly ascending: {} then {}",
                pair[0].version,
                pair[1].version
            );
        }
    }
}
