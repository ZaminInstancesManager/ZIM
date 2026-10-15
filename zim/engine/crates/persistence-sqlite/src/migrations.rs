//! Forward-only migration runner.
//!
//! Migrations are SQL files under `migrations/`, named
//! `NNNN_description.sql`, embedded into the binary at compile time
//! (`include_str!`), and applied in ascending version order inside
//! individual transactions. Per
//! `docs/architecture/RUST_ENGINE.md` §5 there are **no
//! down-migrations**: ZIM's own metadata is small and
//! rollback-via-restore is safer than a schema rollback.
//!
//! Foreign-key enforcement is disabled while a migration applies
//! (table creation order inside a migration is free) and re-enabled
//! afterwards; the port's callers always operate with FKs on.

use zim_engine_application::ports::PersistenceError;

/// One embedded migration.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    /// Monotonic schema version (the `NNNN` prefix).
    pub version: u64,
    /// Short name (the description part of the file name).
    pub name: &'static str,
    /// The SQL body (without transaction control — the runner adds
    /// BEGIN/COMMIT).
    pub sql: &'static str,
}

/// The full migration register, in ascending version order.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial_schema",
        sql: include_str!("../migrations/0001_initial_schema.sql"),
    },
    Migration {
        version: 2,
        name: "jobs_payload",
        sql: include_str!("../migrations/0002_jobs_payload.sql"),
    },
    Migration {
        version: 3,
        name: "eggs",
        sql: include_str!("../migrations/0003_eggs.sql"),
    },
    Migration {
        version: 4,
        name: "instance_variables",
        sql: include_str!("../migrations/0004_instance_variables.sql"),
    },
    Migration {
        version: 5,
        name: "user_recents",
        sql: include_str!("../migrations/0005_user_recents.sql"),
    },
];

/// The highest version this binary knows how to apply.
pub fn max_known_version() -> u64 {
    MIGRATIONS.last().map(|m| m.version).unwrap_or(0)
}

/// Reads the currently applied version from `schema_migrations`
/// (0 for a fresh store; the table is created on demand).
pub fn current_version(conn: &rusqlite::Connection) -> Result<u64, PersistenceError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    INTEGER PRIMARY KEY,
             name       TEXT NOT NULL,
             applied_at TEXT NOT NULL
         )",
    )
    .map_err(|e| PersistenceError::backend(format!("create schema_migrations: {e}")))?;

    conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get::<_, i64>(0),
    )
    .map(|v| v.max(0) as u64)
    .map_err(|e| PersistenceError::backend(format!("read schema version: {e}")))
}

/// Applies every pending migration in order.
///
/// Fails (rolling back the in-flight migration) with
/// [`PersistenceError::MigrationFailed`], or with
/// [`PersistenceError::SchemaTooNew`] when the store was written by
/// a newer engine.
pub fn apply_pending(
    conn: &rusqlite::Connection,
) -> Result<Vec<&'static Migration>, PersistenceError> {
    let current = current_version(conn)?;
    let max_known = max_known_version();

    if current > max_known {
        return Err(PersistenceError::SchemaTooNew {
            found: current,
            supported: max_known,
        });
    }

    // FKs off during application so intra-migration table order is
    // unconstrained; the value is restored before returning.
    let fk_was_on: bool = conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
        .map(|v| v == 1)
        .map_err(|e| PersistenceError::backend(format!("read foreign_keys pragma: {e}")))?;
    conn.pragma_update(None, "foreign_keys", "off")
        .map_err(|e| PersistenceError::backend(format!("disable foreign_keys: {e}")))?;

    let result = apply_pending_inner(conn);

    // Restore the pragma even when a migration failed (the store
    // must not silently stay in FK-off mode).
    let restore = conn
        .pragma_update(None, "foreign_keys", if fk_was_on { "on" } else { "off" })
        .map_err(|e| PersistenceError::backend(format!("restore foreign_keys pragma: {e}")));

    match (result, restore) {
        (Ok(applied), Ok(())) => Ok(applied),
        (Err(e), _) => Err(e),
        (_, Err(e)) => Err(e),
    }
}

fn apply_pending_inner(
    conn: &rusqlite::Connection,
) -> Result<Vec<&'static Migration>, PersistenceError> {
    let mut applied = Vec::new();
    for migration in MIGRATIONS {
        if migration.version <= current_version(conn)? {
            continue;
        }
        conn.execute_batch("BEGIN")
            .map_err(|e| PersistenceError::MigrationFailed {
                version: migration.version,
                name: migration.name.to_owned(),
                reason: format!("begin: {e}"),
            })?;

        let outcome = conn
            .execute_batch(migration.sql)
            .and_then(|_| {
                conn.execute(
                    "INSERT INTO schema_migrations (version, name, applied_at)
                     VALUES (?1, ?2, ?3)",
                    rusqlite::params![
                        migration.version as i64,
                        migration.name,
                        chrono::Utc::now().to_rfc3339()
                    ],
                )
                .map(|_| ())
            })
            .and_then(|_| conn.execute_batch("COMMIT").map(|_| ()));

        if let Err(e) = outcome {
            // Best-effort rollback: the transaction may already have
            // ended if the failure happened at COMMIT time.
            let _ = conn.execute_batch("ROLLBACK");
            return Err(PersistenceError::MigrationFailed {
                version: migration.version,
                name: migration.name.to_owned(),
                reason: e.to_string(),
            });
        }
        applied.push(migration);
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "on").unwrap();
        conn
    }

    #[test]
    fn fresh_store_reports_version_zero() {
        let conn = fresh();
        assert_eq!(current_version(&conn).unwrap(), 0);
    }

    #[test]
    fn applying_all_reaches_max_version() {
        let conn = fresh();
        let applied = apply_pending(&conn).unwrap();
        assert_eq!(applied.len(), MIGRATIONS.len());
        assert_eq!(current_version(&conn).unwrap(), max_known_version());
    }

    #[test]
    fn migrations_are_idempotent() {
        let conn = fresh();
        apply_pending(&conn).unwrap();
        let applied_again = apply_pending(&conn).unwrap();
        assert!(applied_again.is_empty(), "second run must be a no-op");
    }

    #[test]
    fn store_from_newer_engine_is_rejected() {
        let conn = fresh();
        apply_pending(&conn).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![max_known_version() as i64 + 5, "from_the_future", "now"],
        )
        .unwrap();
        match apply_pending(&conn) {
            Err(PersistenceError::SchemaTooNew { found, supported }) => {
                assert_eq!(found, max_known_version() + 5);
                assert_eq!(supported, max_known_version());
            }
            other => panic!("expected SchemaTooNew, got {:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn foreign_keys_pragma_is_restored_after_apply() {
        let conn = fresh();
        apply_pending(&conn).unwrap();
        let fk_on: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fk_on, 1);
    }

    #[test]
    fn entities_table_exists_after_migration() {
        let conn = fresh();
        apply_pending(&conn).unwrap();
        for table in [
            "nodes",
            "users",
            "allocations",
            "instances",
            "permissions",
            "instance_variables",
            "jobs",
            "backups",
            "transfers",
            "schedules",
            "audit",
            "extension_registrations",
            "schema_migrations",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    rusqlite::params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "table {table} should exist after migration");
        }
    }
}
