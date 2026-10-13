//! Service operations and port traits.
//!
//! Defines application services (server creation, lifecycle orchestration,
//! file operations, etc.) using domain types and *port traits* (Rust
//! traits) for persistence and runtime. Concrete adapters implement
//! those traits; this crate depends only on `zim_engine_domain`.
//!
//! # Milestone scope (M3)
//!
//! This crate currently ships the [`Persistence`] port trait
//! (`ports` module) and its [`PersistenceError`] vocabulary, per
//! `docs/architecture/IMPLEMENTATION_ROADMAP.md` §M3. Application
//! *services* (creation orchestration, supervision, ...) arrive with
//! their corresponding milestones (M6+).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod ports;

pub use ports::{Persistence, PersistenceError};

/// The canonical crate name, useful for runtime identification in
/// logs and crash reports.
pub const CRATE_NAME: &str = "zim_engine_application";

#[cfg(test)]
mod tests {
    //! Crate-level smoke tests. Port-trait conformance is verified by
    //! the SQLite adapter's test suite (which exercises the trait
    //! through a concrete implementation).

    use super::*;

    #[test]
    fn crate_name_is_set() {
        assert_eq!(CRATE_NAME, "zim_engine_application");
    }

    #[test]
    fn error_display_messages_are_stable() {
        // The API layer (M5) maps these to structured error bodies;
        // pin the human-readable forms so drift is caught here
        // rather than in API integration tests.
        assert_eq!(
            PersistenceError::backend("disk full").to_string(),
            "storage backend error: disk full"
        );
        assert_eq!(
            PersistenceError::serialization("bad enum").to_string(),
            "serialization error: bad enum"
        );
        assert_eq!(
            PersistenceError::SchemaTooNew {
                found: 3,
                supported: 2
            }
            .to_string(),
            "schema version 3 is newer than the maximum supported 2"
        );
        assert_eq!(
            PersistenceError::MigrationFailed {
                version: 2,
                name: "add_users".to_owned(),
                reason: "syntax error".to_owned()
            }
            .to_string(),
            "migration 2 (add_users) failed: syntax error"
        );
    }
}
