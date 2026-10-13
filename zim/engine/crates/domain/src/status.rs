//! Lifecycle states for instances, jobs, and transfers.
//!
//! # Source of truth
//!
//! `InstanceStatus` mirrors `docs/architecture/RUST_ENGINE.md` §3
//! (the `InstanceStatus` enum). The first four variants (`Offline`,
//! `Starting`, `Running`, `Stopping`) are the base states preserved
//! from Pterodactyl's Wings (per `docs/audits/PTERODACTYL_ARCHITECTURE.md`
//! §7.2.3 — the `ProcessOfflineState`/`ProcessStartingState`/
//! `ProcessRunningState`/`ProcessStoppingState` constants in
//! `upstream/pterodactyl-wings/environment/environment.go`).
//!
//! The remaining variants are ZIM extensions per
//! `docs/architecture/RUST_ENGINE.md` §3 and
//! `docs/architecture/WORKLOAD_ABSTRACTION.md` §6 (extended lifecycle
//! states).
//!
//! `JobState` and `TransferState` mirror the same documents'
//! Job/Transfer state machines.

use serde::{Deserialize, Serialize};

/// The canonical instance/workload lifecycle state.
///
/// The first four variants are the *primary* states preserved from
/// Wings; the remaining variants are ZIM extensions that surface as
/// *substates* of the four primary visual statuses per
/// `docs/architecture/BROWSER_INTEGRATION.md` §3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InstanceStatus {
    /// Observed: not running. May be the desired state (stopped) or the
    /// result of a crash (see `CrashLoop`).
    ///
    /// This is the default per AD-11 (the four-state base lifecycle
    /// preserved from Wings). The engine's restart-reconciliation
    /// logic (invariant 10 in ZIM_ARCHITECTURE.md §4) will reconcile
    /// the persisted default against the runtime's observed state
    /// for each instance on startup.
    #[default]
    Offline,
    /// Process is starting (transitioning from `Offline` to `Running`).
    Starting,
    /// Process is running and observed alive.
    Running,
    /// Process is stopping (transitioning from `Running` to `Offline`).
    Stopping,
    /// Workload is running an install script in a constrained container.
    Installing,
    /// Workload is suspended (no auto-restart on crash).
    Suspended,
    /// Workload is restoring from a backup.
    RestoringBackup,
    /// Workload is being transferred to another node.
    Transferring,
    /// Crashed and exceeds the crash-loop threshold; will not auto-restart.
    /// Per `docs/architecture/RUST_ENGINE.md` §4 invariant 14.
    CrashLoop,
    /// Runtime unreachable / node disconnected. Per
    /// `docs/architecture/ZIM_ARCHITECTURE.md` §5 (failure behavior for
    /// remote-node disconnect): the engine marks the node's instances
    /// as `Unknown`, NOT `Offline` or `Stopped`.
    Unknown,
}

impl InstanceStatus {
    /// Returns true if the status is one of the four primary states
    /// preserved from Wings (`Offline`, `Starting`, `Running`,
    /// `Stopping`).
    ///
    /// Used by tests that verify the four-primary-status visual model
    /// per `docs/architecture/BROWSER_INTEGRATION.md` §3.
    pub fn is_primary(&self) -> bool {
        matches!(
            self,
            Self::Offline | Self::Starting | Self::Running | Self::Stopping
        )
    }

    /// Returns the visual status color name per
    /// `docs/architecture/BROWSER_INTEGRATION.md` §3.
    ///
    /// The four primary visual statuses are:
    /// - `green` — running
    /// - `yellow` — starting / stopping / installing / etc.
    /// - `red` — crashed (with caution glyph for `CrashLoop`)
    /// - `blue` — stopped / idle
    ///
    /// `Unknown` (runtime unreachable / node disconnected) is shown as
    /// `gray` per the failure-behavior matrix in
    /// `docs/architecture/ZIM_ARCHITECTURE.md` §5.
    pub fn visual_color(&self) -> &'static str {
        match self {
            Self::Running => "green",
            Self::Starting
            | Self::Stopping
            | Self::Installing
            | Self::RestoringBackup
            | Self::Transferring => "yellow",
            Self::Offline | Self::Suspended => "blue",
            Self::CrashLoop => "red",
            Self::Unknown => "gray",
        }
    }
}

/// State of a durable job (backup / transfer / install / update).
///
/// Per `docs/architecture/RUST_ENGINE.md` §8 (durable jobs). A job is
/// a state machine; partial failures leave the job in `Failed` with
/// `cleanup_pending = true` per invariant 11 in
/// `docs/architecture/ZIM_ARCHITECTURE.md` §4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Job is queued, not yet started.
    #[default]
    Pending,
    /// Job is currently executing.
    InProgress,
    /// Job completed successfully.
    Succeeded,
    /// Job failed; partial state may require cleanup.
    Failed,
    /// Job was cancelled by the user or admin.
    Cancelled,
    /// Job failed but left partial state on disk that needs a
    /// background cleaner to remove.
    CleanupPending,
}

impl JobState {
    /// Returns true if the job is in a terminal state (no further
    /// transitions expected without external intervention).
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    /// Returns true if the job is in a state where a background
    /// cleaner may need to remove partial state.
    pub fn needs_cleanup(&self) -> bool {
        matches!(self, Self::CleanupPending)
    }
}

/// State of a transfer (two-phase commit).
///
/// Per `docs/architecture/RUST_ENGINE.md` §3 (the `TransferState`
/// enum). The transfer state machine is:
/// `Pending` → `SourceStopping` → `SourceArchiving` →
/// `ArchiveTransferring` → `DestExtracting` → `DestStarting` →
/// `SourceCleanup` → `Succeeded`.
///
/// A partial failure at any stage (other than `SourceCleanup`) leaves
/// the transfer in `Failed` without affecting the source instance's
/// running state per invariant 12 in
/// `docs/architecture/ZIM_ARCHITECTURE.md` §4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TransferState {
    /// Transfer is queued, not yet started.
    #[default]
    Pending,
    /// Stopping the source workload gracefully.
    SourceStopping,
    /// Archiving the source workload's data directory.
    SourceArchiving,
    /// Shipping the archive from source node to destination node.
    ArchiveTransferring,
    /// Extracting the archive on the destination node.
    DestExtracting,
    /// Starting the destination workload.
    DestStarting,
    /// Cleaning up the source workload (deletes source files after
    /// successful destination start).
    SourceCleanup,
    /// Transfer completed successfully.
    Succeeded,
    /// Transfer failed at some stage; partial state may exist on
    /// both source and destination nodes.
    Failed,
    /// Transfer was cancelled by the user or admin.
    Cancelled,
}

impl TransferState {
    /// Returns true if the transfer is in a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&InstanceStatus::CrashLoop).unwrap(),
            "\"crash_loop\""
        );
        assert_eq!(
            serde_json::to_string(&InstanceStatus::RestoringBackup).unwrap(),
            "\"restoring_backup\""
        );
        assert_eq!(
            serde_json::to_string(&InstanceStatus::Unknown).unwrap(),
            "\"unknown\""
        );
    }

    #[test]
    fn status_round_trips_through_serde() {
        let s = InstanceStatus::Running;
        let serialized = serde_json::to_string(&s).unwrap();
        let deserialized: InstanceStatus = serde_json::from_str(&serialized).unwrap();
        assert_eq!(s, deserialized);
    }

    #[test]
    fn primary_states_are_the_four_wings_states() {
        assert!(InstanceStatus::Offline.is_primary());
        assert!(InstanceStatus::Starting.is_primary());
        assert!(InstanceStatus::Running.is_primary());
        assert!(InstanceStatus::Stopping.is_primary());

        // Extended states are NOT primary.
        assert!(!InstanceStatus::Installing.is_primary());
        assert!(!InstanceStatus::Suspended.is_primary());
        assert!(!InstanceStatus::RestoringBackup.is_primary());
        assert!(!InstanceStatus::Transferring.is_primary());
        assert!(!InstanceStatus::CrashLoop.is_primary());
        assert!(!InstanceStatus::Unknown.is_primary());
    }

    #[test]
    fn visual_color_mappings() {
        assert_eq!(InstanceStatus::Running.visual_color(), "green");
        assert_eq!(InstanceStatus::Starting.visual_color(), "yellow");
        assert_eq!(InstanceStatus::Stopping.visual_color(), "yellow");
        assert_eq!(InstanceStatus::Installing.visual_color(), "yellow");
        assert_eq!(InstanceStatus::RestoringBackup.visual_color(), "yellow");
        assert_eq!(InstanceStatus::Transferring.visual_color(), "yellow");
        assert_eq!(InstanceStatus::Offline.visual_color(), "blue");
        assert_eq!(InstanceStatus::Suspended.visual_color(), "blue");
        assert_eq!(InstanceStatus::CrashLoop.visual_color(), "red");
        assert_eq!(InstanceStatus::Unknown.visual_color(), "gray");
    }

    #[test]
    fn default_status_is_offline() {
        // Per AD-11 in docs/architecture/DECISIONS.md.
        assert_eq!(InstanceStatus::default(), InstanceStatus::Offline);
    }

    #[test]
    fn job_state_terminal_check() {
        assert!(JobState::Succeeded.is_terminal());
        assert!(JobState::Failed.is_terminal());
        assert!(JobState::Cancelled.is_terminal());
        assert!(!JobState::Pending.is_terminal());
        assert!(!JobState::InProgress.is_terminal());
        assert!(!JobState::CleanupPending.is_terminal());
    }

    #[test]
    fn job_state_cleanup_check() {
        assert!(JobState::CleanupPending.needs_cleanup());
        assert!(!JobState::Succeeded.needs_cleanup());
        assert!(!JobState::Failed.needs_cleanup());
    }

    #[test]
    fn transfer_state_terminal_check() {
        assert!(TransferState::Succeeded.is_terminal());
        assert!(TransferState::Failed.is_terminal());
        assert!(TransferState::Cancelled.is_terminal());
        assert!(!TransferState::Pending.is_terminal());
        assert!(!TransferState::SourceStopping.is_terminal());
        assert!(!TransferState::DestStarting.is_terminal());
    }

    #[test]
    fn transfer_state_default_is_pending() {
        assert_eq!(TransferState::default(), TransferState::Pending);
    }

    #[test]
    fn job_state_default_is_pending() {
        assert_eq!(JobState::default(), JobState::Pending);
    }
}
