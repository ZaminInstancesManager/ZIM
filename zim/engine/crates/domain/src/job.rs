//! Durable Job, Backup, and Transfer entities.
//!
//! # Source of truth
//!
//! `Job`, `JobKind`, `Backup`, and `Transfer` mirror
//! `docs/architecture/RUST_ENGINE.md` §3 and §8 (durable jobs). The
//! state machines (`JobState`, `TransferState`) already live in the
//! `status` module.
//!
//! Invariants of note (from `docs/architecture/ZIM_ARCHITECTURE.md`
//! §4): partial failures leave a job in `Failed` with
//! `cleanup_pending = true` (invariant 11); a failed transfer must
//! not affect the source instance's running state (invariant 12).

use crate::id::{BackupId, InstanceId, JobId, NodeId, TransferId};
use crate::status::{JobState, TransferState};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The kind of durable job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// Create (or restore) an archive of an instance's data.
    Backup,
    /// Move an instance between nodes (two-phase commit).
    Transfer,
    /// Run an egg install script in a constrained container.
    Install,
    /// Update an instance's egg variables / image / startup.
    Update,
}

/// A durable, resumable background job.
///
/// Jobs survive engine restarts: on startup the engine resumes
/// `Pending`/`InProgress` jobs or moves them to a failed state with
/// `cleanup_pending` per the durable-jobs semantics in
/// `docs/architecture/RUST_ENGINE.md` §8.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    /// Strongly-typed job identifier.
    pub id: JobId,
    /// The kind of work this job performs.
    pub kind: JobKind,
    /// The instance the job operates on.
    pub instance_id: InstanceId,
    /// Current state-machine state.
    pub state: JobState,
    /// Row creation time.
    pub created_at: DateTime<Utc>,
    /// Row last-update time.
    pub updated_at: DateTime<Utc>,
    /// When execution started (None while queued).
    pub started_at: Option<DateTime<Utc>>,
    /// When execution reached a terminal state.
    pub completed_at: Option<DateTime<Utc>>,
    /// The last error message, for `Failed`/`CleanupPending` states.
    pub last_error: Option<String>,
    /// Whether partial on-disk state needs the background cleaner.
    pub cleanup_pending: bool,
    /// Monotonic generation counter for stale-event protection
    /// (events carrying an older generation than the persisted job
    /// are dropped per the streaming spec).
    pub generation: u64,
    /// Kind-specific job target, as opaque JSON text (M8): e.g. a
    /// restore job names the backup id it restores. `None` for jobs
    /// whose companion entity row already carries the linkage (a
    /// backup-create job links through the `Backup` row's
    /// `job_id`).
    #[serde(default)]
    pub payload: Option<String>,
}

impl Job {
    /// Whether this job is in a terminal state.
    pub fn is_terminal(&self) -> bool {
        self.state.is_terminal()
    }
}

/// A completed (or in-progress) backup archive.
///
/// Linked to the durable [`Job`] that created it: the archive's
/// integrity (sha256) is recorded at creation time and re-verified
/// on restore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Backup {
    /// Strongly-typed backup identifier.
    pub id: BackupId,
    /// The instance this backup belongs to.
    pub instance_id: InstanceId,
    /// The durable job that created this backup.
    pub job_id: JobId,
    /// Backup UUID (file name stem / API identifier).
    pub uuid: String,
    /// Archive file name on disk.
    pub file_name: String,
    /// Archive size in bytes.
    pub bytes: u64,
    /// sha256 hex digest of the archive (None while writing).
    pub sha256_hash: Option<String>,
    /// Whether the backup is locked against deletion.
    pub locked: bool,
    /// When the archive was sealed (None while in progress).
    pub completed_at: Option<DateTime<Utc>>,
}

/// An instance transfer between nodes (two-phase commit).
///
/// The state machine lives in [`TransferState`]; this struct carries
/// the node pair and the intermediate archive location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transfer {
    /// Strongly-typed transfer identifier.
    pub id: TransferId,
    /// The instance being transferred.
    pub instance_id: InstanceId,
    /// The durable job driving this transfer.
    pub job_id: JobId,
    /// Source node.
    pub from_node_id: NodeId,
    /// Destination node.
    pub to_node_id: NodeId,
    /// Current two-phase-commit state.
    pub state: TransferState,
    /// Where the intermediate archive sits (node-local path), while
    /// the transfer is in flight.
    pub archive_path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_job() -> Job {
        Job {
            id: JobId::new(),
            kind: JobKind::Backup,
            instance_id: InstanceId::new(),
            state: JobState::InProgress,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            started_at: Some(Utc::now()),
            completed_at: None,
            last_error: None,
            cleanup_pending: false,
            generation: 7,
            payload: None,
        }
    }

    #[test]
    fn job_round_trips_through_serde() {
        let job = sample_job();
        let json = serde_json::to_string(&job).unwrap();
        let back: Job = serde_json::from_str(&json).unwrap();
        assert_eq!(job, back);
    }

    #[test]
    fn failed_job_with_cleanup_round_trips() {
        let mut job = sample_job();
        job.state = JobState::CleanupPending;
        job.cleanup_pending = true;
        job.last_error = Some("archive write interrupted".to_owned());
        job.completed_at = Some(Utc::now());
        let json = serde_json::to_string(&job).unwrap();
        let back: Job = serde_json::from_str(&json).unwrap();
        assert_eq!(job, back);
        assert!(back.cleanup_pending);
        assert_eq!(
            back.last_error.as_deref(),
            Some("archive write interrupted")
        );
    }

    #[test]
    fn job_is_terminal_reflects_state() {
        let mut job = sample_job();
        job.state = JobState::Pending;
        assert!(!job.is_terminal());
        job.state = JobState::Succeeded;
        assert!(job.is_terminal());
    }

    #[test]
    fn job_kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&JobKind::Backup).unwrap(),
            "\"backup\""
        );
        assert_eq!(
            serde_json::to_string(&JobKind::Transfer).unwrap(),
            "\"transfer\""
        );
        assert_eq!(
            serde_json::to_string(&JobKind::Install).unwrap(),
            "\"install\""
        );
        assert_eq!(
            serde_json::to_string(&JobKind::Update).unwrap(),
            "\"update\""
        );
    }

    fn sample_backup() -> Backup {
        Backup {
            id: BackupId::new(),
            instance_id: InstanceId::new(),
            job_id: JobId::new(),
            uuid: "6ba7b810-9dad-11d1-80b4-00c04fd430c8".to_owned(),
            file_name: "6ba7b810-9dad-11d1-80b4-00c04fd430c8.tar.gz".to_owned(),
            bytes: 104_857_600,
            sha256_hash: Some(
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_owned(),
            ),
            locked: true,
            completed_at: Some(Utc::now()),
        }
    }

    #[test]
    fn backup_round_trips_through_serde() {
        let backup = sample_backup();
        let json = serde_json::to_string(&backup).unwrap();
        let back: Backup = serde_json::from_str(&json).unwrap();
        assert_eq!(backup, back);
    }

    #[test]
    fn backup_in_progress_has_no_hash_nor_completion() {
        let mut backup = sample_backup();
        backup.sha256_hash = None;
        backup.completed_at = None;
        let json = serde_json::to_string(&backup).unwrap();
        let back: Backup = serde_json::from_str(&json).unwrap();
        assert_eq!(back.sha256_hash, None);
        assert_eq!(back.completed_at, None);
    }

    fn sample_transfer() -> Transfer {
        Transfer {
            id: TransferId::new(),
            instance_id: InstanceId::new(),
            job_id: JobId::new(),
            from_node_id: NodeId::new(),
            to_node_id: NodeId::new(),
            state: TransferState::ArchiveTransferring,
            archive_path: Some("/var/lib/zim/transfers/tmp-abc.tar.gz".to_owned()),
        }
    }

    #[test]
    fn transfer_round_trips_through_serde() {
        let transfer = sample_transfer();
        let json = serde_json::to_string(&transfer).unwrap();
        let back: Transfer = serde_json::from_str(&json).unwrap();
        assert_eq!(transfer, back);
    }

    #[test]
    fn transfer_full_state_machine_round_trips() {
        let states = [
            TransferState::Pending,
            TransferState::SourceStopping,
            TransferState::SourceArchiving,
            TransferState::ArchiveTransferring,
            TransferState::DestExtracting,
            TransferState::DestStarting,
            TransferState::SourceCleanup,
            TransferState::Succeeded,
            TransferState::Failed,
            TransferState::Cancelled,
        ];
        for state in states {
            let mut transfer = sample_transfer();
            transfer.state = state;
            let json = serde_json::to_string(&transfer).unwrap();
            let back: Transfer = serde_json::from_str(&json).unwrap();
            assert_eq!(back.state, state);
        }
    }
}
