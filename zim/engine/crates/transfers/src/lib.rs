//! Transfer domain: the two-phase-commit instance move.
//!
//! A transfer moves an instance from one node to another as a
//! durable job (roadmap M8.2, `docs/architecture/RUST_ENGINE.md`
//! §8): the source side stops the workload and archives its data
//! directory; the destination side receives the archive, verifies
//! its digest, extracts it into the live directory, rebinds the
//! instance's node (and allocation), and starts the workload; the
//! source side then cleans up. The persisted
//! [`zim_engine_domain::status::TransferState`] walks
//! `Pending → SourceStopping → SourceArchiving →
//! ArchiveTransferring → DestExtracting → DestStarting →
//! SourceCleanup → Succeeded`, and a partial failure at any stage
//! leaves the job in `Failed` with `cleanup_pending = true` per
//! invariant 11 — the background cleaner removes the transfer's
//! staging tree — while the source instance stays runnable (its
//! data is never replaced without a digest-verified copy staged
//! first, and its pre-transfer power state is restored on the way
//! out of a failure).
//!
//! # Layout
//!
//! - [`store`] — the on-disk layout of a transfer's working tree
//!   under `{data_root}/transfers/{instance_short}/{transfer_id}/`.
//! - [`service`] — the API-facing `TransferService`: validation,
//!   `Transfer` + `Job` row creation, enqueue, cancel, status.
//! - [`handler`] — the `JobKind::Transfer` handler: the state
//!   machine's executable half, incl. the cleaner's cleanup.
//!
//! # Engine-local execution (this milestone)
//!
//! Both phases execute under the engine's own data root: the
//! archive is created on the source side of the tree and moved to
//! the destination side (a same-filesystem rename), then verified
//! and extracted. This is deliberate: the state machine, the
//! persisted row lifecycle, and the failure semantics are the
//! milestone; the remote transport (the node-runtime contract,
//! `docs/architecture/RUST_ENGINE.md` §9) replaces the local move
//! inside the `ArchiveTransferring` step without changing any
//! state or invariant around it.
//!
//! # Architecture boundary
//!
//! Depends on the domain, application (persistence port +
//! supervision's power queue), jobs (the `JobHandler` contract),
//! and backups (the shared archive engine) layers. The permitted
//! dependency direction is enforced at compile time by the
//! workspace crate graph (`docs/architecture/RUST_ENGINE.md`
//! §2).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod handler;
mod service;
mod store;

pub use handler::{TransferJobHandler, TransferPhasePolicy};
pub use service::{TransferError, TransferService};
pub use store::TransferStore;

/// The canonical crate name.
pub const CRATE_NAME: &str = "zim_engine_transfers";

#[cfg(test)]
mod tests {
    use super::CRATE_NAME;

    #[test]
    fn crate_name_is_set() {
        assert_eq!(CRATE_NAME, "zim_engine_transfers");
    }
}
