//! Backup domain.
//!
//! Archive-based backups (file-level) and the executable machinery
//! around them. Backup is a durable job; see
//! `docs/architecture/RUST_ENGINE.md` §8.
//!
//! # Layout
//!
//! - [`archive`] — the tar+gzip engine: create (streaming sha256,
//!   atomic `.part` → rename finalize), verify, and restore
//!   (digest-checked, escape-safe extraction).
//! - [`store`] — the on-disk layout of archives under
//!   `{data_root}/backups/{instance_uuid_short}/`.
//! - [`service`] — the API-facing `BackupService`: limit
//!   enforcement, row creation, job dispatch, delete / lock.
//! - [`handler`] — the `JobKind::Backup` handler (create and
//!   restore directions) incl. the cleaner's cleanup.
//!
//! # Failure semantics (roadmap M8)
//!
//! An interrupted backup leaves the job in `CleanupPending` with
//! the partial `.part` archive on disk; the job system's background
//! cleaner calls the handler's cleanup and the job lands in
//! `Failed`. A restore extracts into a staging directory and swaps
//! it into place, so a failed restore never corrupts the live data
//! directory (and rolls back the swap on failure).
//!
//! # Architecture boundary
//!
//! Depends on the domain, application (persistence port +
//! `instance_data_dir`), and jobs (the `JobHandler` contract)
//! layers. The permitted dependency direction is enforced at
//! compile time by the workspace crate graph
//! (`docs/architecture/RUST_ENGINE.md` §2).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod archive;
mod handler;
mod service;
mod store;
pub use archive::{
    create_archive, hash_file, part_path_for, restore_archive, verify_archive, ArchiveError,
    ArchiveResult,
};
pub use handler::BackupJobHandler;
pub use service::{BackupError, BackupService};
pub use store::ArchiveStore;

/// The canonical crate name.
pub const CRATE_NAME: &str = "zim_engine_backups";
