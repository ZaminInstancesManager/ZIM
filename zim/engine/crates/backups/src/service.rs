//! The backup service: row management + job dispatch for the backup
//! API surface.
//!
//! [`BackupService`] is the API-facing half: it enforces the
//! instance's `backup_limit`, creates the linked `Backup` + `Job`
//! rows, enqueues the job on the [`JobSystem`], and implements
//! delete / lock / unlock against the persisted rows and the
//! archive files. The executable half — what a backup job actually
//! does to bytes on disk — is [`crate::handler::BackupJobHandler`],
//! registered by [`BackupService::register`].

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use uuid::Uuid;

use zim_engine_application::ports::Persistence;
use zim_engine_domain::job::{Job, JobKind};
use zim_engine_domain::status::JobState;
use zim_engine_domain::{Backup, BackupId, InstanceId, JobId};

use crate::archive::part_path_for;
use crate::handler::BackupJobHandler;
use crate::store::ArchiveStore;
use zim_engine_jobs::JobSystem;

/// Service-level failures (mapped by the API layer onto §6.1 error
/// codes).
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    /// The store failed.
    #[error("persistence: {0}")]
    Persistence(#[from] zim_engine_application::ports::PersistenceError),
    /// No instance with that id.
    #[error("instance {0} not found")]
    InstanceNotFound(InstanceId),
    /// No backup with that id (or not on this instance).
    #[error("backup {0} not found")]
    BackupNotFound(BackupId),
    /// The instance's backup count is at its limit.
    #[error("backup limit reached ({limit} backups exist)")]
    LimitReached {
        /// The configured limit.
        limit: u32,
    },
    /// The backup is locked against deletion.
    #[error("backup {0} is locked")]
    Locked(BackupId),
    /// The backup is still being written (no sha256 yet).
    #[error("backup {0} is not completed yet")]
    NotCompleted(BackupId),
    /// The archive file is missing on disk.
    #[error("backup archive {file_name} is missing on disk")]
    MissingArchive {
        /// The recorded file name.
        file_name: String,
    },
}

/// The backup API's service object (see the module docs).
pub struct BackupService {
    persistence: Arc<dyn Persistence>,
    jobs: Arc<JobSystem>,
    store: ArchiveStore,
    data_root: PathBuf,
}

impl BackupService {
    /// Build the service; call [`BackupService::register`] before
    /// jobs start flowing.
    pub fn new(
        persistence: Arc<dyn Persistence>,
        jobs: Arc<JobSystem>,
        data_root: PathBuf,
    ) -> Arc<Self> {
        Arc::new(Self {
            store: ArchiveStore::new(&data_root),
            persistence,
            jobs,
            data_root,
        })
    }

    /// Register the backup job handler on the executor.
    pub async fn register(&self) {
        self.jobs
            .register_handler(
                JobKind::Backup,
                Arc::new(BackupJobHandler::new(
                    Arc::clone(&self.persistence),
                    self.store.clone(),
                    self.data_root.clone(),
                )),
            )
            .await;
    }

    /// Start a backup of an instance: creates the in-progress
    /// `Backup` row and the linked `Job`, enqueues it, and returns
    /// both (the API answers with the job id; the backup row
    /// completes when the job does).
    pub async fn create_backup(
        &self,
        instance_id: InstanceId,
    ) -> Result<(Job, Backup), BackupError> {
        let persistence = Arc::clone(&self.persistence);
        let instance = tokio::task::spawn_blocking({
            let id = instance_id;
            move || persistence.get_instance(&id)
        })
        .await
        .expect("backup instance read task panicked")?
        .ok_or(BackupError::InstanceNotFound(instance_id))?;

        // Enforce the instance's backup limit (0 = unlimited).
        let persistence = Arc::clone(&self.persistence);
        let existing = tokio::task::spawn_blocking({
            let id = instance_id;
            move || persistence.list_backups_by_instance(&id)
        })
        .await
        .expect("backup count task panicked")?;
        if instance.backup_limit > 0 && existing.len() >= instance.backup_limit as usize {
            return Err(BackupError::LimitReached {
                limit: instance.backup_limit,
            });
        }

        let now = Utc::now();
        let backup_uuid = Uuid::new_v4().to_string();
        let job = Job {
            id: JobId::new(),
            kind: JobKind::Backup,
            instance_id,
            state: JobState::Pending,
            created_at: now,
            updated_at: now,
            started_at: None,
            completed_at: None,
            last_error: None,
            cleanup_pending: false,
            generation: 0,
            // Create direction: no payload (the Backup row's job_id
            // is the linkage).
            payload: None,
        };
        let backup = Backup {
            id: BackupId::new(),
            instance_id,
            job_id: job.id,
            uuid: backup_uuid.clone(),
            file_name: format!("{backup_uuid}.tar.gz"),
            bytes: 0,
            sha256_hash: None,
            locked: false,
            completed_at: None,
        };

        // Job row first (the backup row's FK points at it), then the
        // backup row; the enqueue's put_job is an idempotent upsert
        // that also wakes the queue. The handler finds the backup by
        // job id, so both rows exist before it can run.
        let persistence = Arc::clone(&self.persistence);
        let job_row = job.clone();
        tokio::task::spawn_blocking(move || persistence.put_job(&job_row))
            .await
            .expect("job row task panicked")?;
        let persistence = Arc::clone(&self.persistence);
        let backup_row = backup.clone();
        tokio::task::spawn_blocking(move || persistence.put_backup(&backup_row))
            .await
            .expect("backup row task panicked")?;

        let job = self.jobs.enqueue(job).await?;
        Ok((job, backup))
    }

    /// Start a restore of a completed backup. The restore runs as
    /// the same `JobKind::Backup` kind; the handler discriminates
    /// by the linked row's `sha256_hash` (a `Some` digest means
    /// "restore me").
    pub async fn restore_backup(
        &self,
        instance_id: InstanceId,
        backup_id: BackupId,
    ) -> Result<(Job, Backup), BackupError> {
        let persistence = Arc::clone(&self.persistence);
        let backup = tokio::task::spawn_blocking({
            let id = backup_id;
            move || persistence.get_backup(&id)
        })
        .await
        .expect("restore backup read task panicked")?
        .ok_or(BackupError::BackupNotFound(backup_id))?;
        if backup.instance_id != instance_id {
            return Err(BackupError::BackupNotFound(backup_id));
        }
        if backup.sha256_hash.is_none() {
            return Err(BackupError::NotCompleted(backup_id));
        }
        let instance_short = self.instance_short(instance_id).await?;
        let archive = self.store.archive_path(&instance_short, &backup);
        if !archive.exists() {
            return Err(BackupError::MissingArchive {
                file_name: backup.file_name.clone(),
            });
        }

        let now = Utc::now();
        let job = Job {
            id: JobId::new(),
            kind: JobKind::Backup,
            instance_id,
            state: JobState::Pending,
            created_at: now,
            updated_at: now,
            started_at: None,
            completed_at: None,
            last_error: None,
            cleanup_pending: false,
            generation: 0,
            // The restore direction's linkage: the payload names the
            // backup being restored (no companion row is created for
            // a restore).
            payload: Some(serde_json::json!({ "backup_id": backup.id.to_string() }).to_string()),
        };
        let job = self.jobs.enqueue(job).await?;
        Ok((job, backup))
    }

    /// Delete a backup: its archive file and its row. Locked
    /// backups refuse (the §10.5 semantics); a missing file is not
    /// an error (the archive storage may have been pruned).
    pub async fn delete_backup(
        &self,
        instance_id: InstanceId,
        backup_id: BackupId,
    ) -> Result<(), BackupError> {
        let persistence = Arc::clone(&self.persistence);
        let backup = tokio::task::spawn_blocking({
            let id = backup_id;
            move || persistence.get_backup(&id)
        })
        .await
        .expect("delete backup read task panicked")?
        .ok_or(BackupError::BackupNotFound(backup_id))?;
        if backup.instance_id != instance_id {
            return Err(BackupError::BackupNotFound(backup_id));
        }
        if backup.locked {
            return Err(BackupError::Locked(backup_id));
        }

        let instance_short = self.instance_short(instance_id).await?;

        // File first, row second: a crash between leaves a row
        // without its file, which `restore` reports cleanly; the
        // reverse would promise bytes we no longer have.
        let archive = self.store.archive_path(&instance_short, &backup);
        let part = part_path_for(&archive);
        for path in [&archive, &part] {
            if path.exists() {
                let result = tokio::task::spawn_blocking({
                    let path = path.clone();
                    move || std::fs::remove_file(&path)
                })
                .await
                .expect("archive delete task panicked");
                // A vanished file is fine (pruned storage); a real
                // I/O error is not.
                if let Err(e) = result {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        return Err(BackupError::Persistence(
                            zim_engine_application::ports::PersistenceError::backend(format!(
                                "cannot delete archive {}: {e}",
                                path.display()
                            )),
                        ));
                    }
                }
            }
        }

        let persistence = Arc::clone(&self.persistence);
        let id = backup_id;
        tokio::task::spawn_blocking(move || persistence.delete_backup(&id))
            .await
            .expect("backup row delete task panicked")?;
        Ok(())
    }

    /// Lock or unlock a backup against deletion.
    pub async fn set_locked(
        &self,
        instance_id: InstanceId,
        backup_id: BackupId,
        locked: bool,
    ) -> Result<Backup, BackupError> {
        let persistence = Arc::clone(&self.persistence);
        let mut backup = tokio::task::spawn_blocking({
            let id = backup_id;
            move || persistence.get_backup(&id)
        })
        .await
        .expect("lock backup read task panicked")?
        .ok_or(BackupError::BackupNotFound(backup_id))?;
        if backup.instance_id != instance_id {
            return Err(BackupError::BackupNotFound(backup_id));
        }
        backup.locked = locked;
        let persistence = Arc::clone(&self.persistence);
        let row = backup.clone();
        tokio::task::spawn_blocking(move || persistence.put_backup(&row))
            .await
            .expect("lock backup write task panicked")?;
        Ok(backup)
    }

    /// The instance's short UUID (the archive-directory key).
    async fn instance_short(&self, instance_id: InstanceId) -> Result<String, BackupError> {
        let persistence = Arc::clone(&self.persistence);
        let instance = tokio::task::spawn_blocking({
            let id = instance_id;
            move || persistence.get_instance(&id)
        })
        .await
        .expect("instance short read task panicked")?
        .ok_or(BackupError::InstanceNotFound(instance_id))?;
        Ok(instance.uuid_short)
    }

    /// Resolve a backup's on-disk archive path (the store layout
    /// keys on the instance's short UUID). Fails when either row is
    /// missing or the backup belongs to another instance.
    pub async fn archive_path_for(
        &self,
        instance_id: InstanceId,
        backup_id: BackupId,
    ) -> Result<PathBuf, BackupError> {
        let persistence = Arc::clone(&self.persistence);
        let backup = tokio::task::spawn_blocking({
            let id = backup_id;
            move || persistence.get_backup(&id)
        })
        .await
        .expect("archive path backup read task panicked")?
        .ok_or(BackupError::BackupNotFound(backup_id))?;
        if backup.instance_id != instance_id {
            return Err(BackupError::BackupNotFound(backup_id));
        }
        let short = self.instance_short(instance_id).await?;
        Ok(self.store.archive_path(&short, &backup))
    }
}

impl From<zim_engine_jobs::JobsError> for BackupError {
    fn from(e: zim_engine_jobs::JobsError) -> Self {
        Self::Persistence(zim_engine_application::ports::PersistenceError::backend(
            format!("job system: {e}"),
        ))
    }
}
