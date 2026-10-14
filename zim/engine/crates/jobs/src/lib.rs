//! Durable jobs (backup / transfer / install / update).
//!
//! # What lives here
//!
//! The *executor* half of `docs/architecture/RUST_ENGINE.md` §8: the
//! [`JobSystem`] claims `Pending` job rows, runs the registered
//! [`JobHandler`] for each job's kind under per-instance
//! serialization and a global concurrency cap, persists every state
//! transition, publishes `job.progress` / `job.completed` events
//! onto the streaming hub, reconciles jobs orphaned by an engine
//! crash at boot, and runs the background cleaner that removes the
//! partial on-disk state of `CleanupPending` jobs.
//!
//! The *executable* halves — what a backup, a transfer, or an
//! install actually do — live in their own crates
//! (`zim_engine_backups`, `zim_engine_transfers`) and register
//! themselves with the executor at composition time.
//!
//! # State machine
//!
//! ```text
//!            enqueue                 claim (persisted)
//!  (caller) ─────────▶ Pending ─────────────────────▶ InProgress
//!                        │                               │
//!      cancel (Pending)  │                               │ handler outcome
//!                        ▼                               ▼
//!                   Cancelled            Succeeded / Cancelled / Failed
//!                        │                               │
//!                        │                    Failed{cleanup_pending}
//!                        │                               │
//!                        │                               ▼
//!                        └──────────── terminal ◀── CleanupPending
//!                                          (cleaner ran
//!                                           handler.cleanup)
//! ```
//!
//! `Pending` and `InProgress` rows survive an engine restart:
//! `Pending` was never claimed (the queue picks it up again);
//! `InProgress` belongs to the dead process, so
//! [`JobSystem::reconcile_orphans`] moves it to `CleanupPending`
//! and the cleaner finishes the story (invariant 11 in
//! `docs/architecture/ZIM_ARCHITECTURE.md` §4).
//!
//! # Architecture boundary
//!
//! This crate sits beside the application layer's supervision module
//! (both orchestrate long-running work) and, like it, depends on the
//! domain + streaming layers. The permitted dependency direction is
//! enforced at compile time by the workspace crate graph
//! (`docs/architecture/RUST_ENGINE.md` §2); concrete job handlers
//! register from their own crates, so this crate never depends on
//! `zim_engine_backups` or `zim_engine_transfers`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod executor;
mod handler;

pub use executor::{CancelStatus, JobPolicy, JobSystem, JobsError};
pub use handler::{CancelToken, JobContext, JobHandler, JobOutcome, ProgressFn};

/// The canonical crate name.
pub const CRATE_NAME: &str = "zim_engine_jobs";
