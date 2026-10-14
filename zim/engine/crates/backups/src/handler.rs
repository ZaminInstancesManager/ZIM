//! The executable half of the backup system: the `JobKind::Backup`
//! handler.
//!
//! One handler serves both directions, discriminated by the linked
//! [`Backup`] row's `sha256_hash` (the domain's `JobKind::Backup`
//! covers "create **or restore** an archive"):
//!
//! - `sha256_hash == None` → **create**: archive the instance's data
//!   directory into the store, then record `bytes` + `sha256_hash` +
//!   `completed_at` on the row.
//! - `sha256_hash == Some(_)` → **restore**: verify the recorded
//!   digest, extract into a staging directory beside the instance
//!   data, then replace the live directory's *contents* in place
//!   (the directory itself is never renamed — a live container's
//!   bind mount pins it).
//!
//! Failure semantics (roadmap M8 acceptance): a failed or cancelled
//! create leaves the `.part` file and answers
//! [`JobOutcome::Failed`] with `cleanup_pending = true`; a failed
//! restore leaves the staging directory the same
//! way. [`JobHandler::cleanup`] removes both — including for jobs
//! orphaned by an engine crash, which boot reconciliation moves to
//! `CleanupPending` precisely so this runs.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;

use zim_engine_application::ports::Persistence;
use zim_engine_application::supervision::instance_data_dir;
use zim_engine_domain::job::Job;
use zim_engine_domain::{Backup, Instance};

use zim_engine_jobs::{JobContext, JobHandler, JobOutcome};

use crate::archive::{restore_archive, ArchiveError};
use crate::store::ArchiveStore;

/// The backup job handler (see the module docs).
pub struct BackupJobHandler {
    persistence: Arc<dyn Persistence>,
    store: ArchiveStore,
    data_root: PathBuf,
}

impl BackupJobHandler {
    /// Build the handler; [`crate::BackupService::register`] installs
    /// it on the executor.
    pub fn new(persistence: Arc<dyn Persistence>, store: ArchiveStore, data_root: PathBuf) -> Self {
        Self {
            persistence,
            store,
            data_root,
        }
    }

    /// The linked backup row for a create job (None when the row was
    /// deleted — e.g. a canceled create whose backup row the API
    /// already removed).
    fn backup_for_job_sync(persistence: &dyn Persistence, job: &Job) -> Option<Backup> {
        let backups = persistence
            .list_backups_by_instance(&job.instance_id)
            .ok()?;
        backups.into_iter().find(|b| b.job_id == job.id)
    }

    /// The backup row a restore job targets (from the job's JSON
    /// `payload`: `{"backup_id": "..."}`).
    fn restore_target_sync(persistence: &dyn Persistence, job: &Job) -> Option<Backup> {
        let payload = job.payload.as_deref()?;
        let value: serde_json::Value = serde_json::from_str(payload).ok()?;
        let backup_id = value.get("backup_id")?.as_str()?;
        let id: zim_engine_domain::BackupId = backup_id.parse().ok()?;
        persistence.get_backup(&id).ok().flatten()
    }

    /// Load the job's instance (None when the instance was deleted
    /// out from under the job).
    fn instance_sync(
        persistence: &dyn Persistence,
        instance_id: &zim_engine_domain::InstanceId,
    ) -> Option<Instance> {
        persistence.get_instance(instance_id).ok().flatten()
    }
}

#[async_trait]
impl JobHandler for BackupJobHandler {
    async fn run(&self, ctx: JobContext) -> JobOutcome {
        let job = ctx.job().clone();
        let backup = tokio::task::spawn_blocking({
            let persistence = Arc::clone(&self.persistence);
            let job = job.clone();
            move || {
                // Create direction: the companion row links to this
                // job. Restore direction: the payload names the
                // target backup.
                Self::backup_for_job_sync(&*persistence, &job)
                    .or_else(|| Self::restore_target_sync(&*persistence, &job))
            }
        })
        .await
        .expect("backup row lookup task panicked");

        let backup = match backup {
            Some(backup) => backup,
            None => {
                // No linked row and no resolvable target: nothing to
                // do, nothing partial on disk by our conventions.
                return JobOutcome::Failed {
                    message: "the backup this job operates on no longer exists".to_owned(),
                    cleanup_pending: false,
                };
            }
        };

        let instance = tokio::task::spawn_blocking({
            let persistence = Arc::clone(&self.persistence);
            let instance_id = job.instance_id;
            move || Self::instance_sync(&*persistence, &instance_id)
        })
        .await
        .expect("instance lookup task panicked");
        let instance = match instance {
            Some(instance) => instance,
            None => {
                return JobOutcome::Failed {
                    message: "the job's instance no longer exists".to_owned(),
                    cleanup_pending: false,
                };
            }
        };

        if backup.sha256_hash.is_none() {
            self.run_create(ctx, &backup, &instance).await
        } else {
            self.run_restore(ctx, &job, &backup, &instance).await
        }
    }

    async fn cleanup(&self, job: &Job) -> Result<(), String> {
        // Everything this handler can leave behind is derived from
        // the instance row + the linked backup row: the `.part` file
        // (create direction) and the job-tagged staging
        // directories (restore direction).
        let instance = tokio::task::spawn_blocking({
            let persistence = Arc::clone(&self.persistence);
            let instance_id = job.instance_id;
            move || Self::instance_sync(&*persistence, &instance_id)
        })
        .await
        .expect("cleanup instance lookup task panicked");

        let Some(instance) = instance else {
            // The instance row is gone, so its uuid_short — the key
            // to every path this handler writes — is unknowable.
            // Nothing derivable remains to clean; unreachable
            // archives under the store root are the instance-
            // deletion flow's concern, not the cleaner's.
            return Ok(());
        };
        let short = instance.uuid_short;

        let backup = tokio::task::spawn_blocking({
            let persistence = Arc::clone(&self.persistence);
            let job = job.clone();
            move || Self::backup_for_job_sync(&*persistence, &job)
        })
        .await
        .expect("cleanup backup lookup task panicked");

        // Create-direction leftover.
        if let Some(backup) = &backup {
            let part = self.store.part_path(&short, backup);
            if part.exists() {
                std::fs::remove_file(&part)
                    .map_err(|e| format!("cannot remove {}: {e}", part.display()))?;
            }
        }

        // Restore-direction leftovers (job-tagged, so they are
        // derivable without the backup row).
        let staging = self.staging_dir(&short, &job.id.to_string());
        if staging.exists() {
            std::fs::remove_dir_all(&staging)
                .map_err(|e| format!("cannot remove {}: {e}", staging.display()))?;
        }
        Ok(())
    }
}

impl BackupJobHandler {
    /// Staging directory for a restore: a hidden sibling of the
    /// instance data dir, tagged by job id.
    fn staging_dir(&self, instance_short: &str, tag: &str) -> PathBuf {
        self.data_root
            .join("instances")
            .join(format!(".{instance_short}.restoring.{tag}"))
    }

    /// The create direction (see the module docs).
    async fn run_create(
        &self,
        ctx: JobContext,
        backup: &Backup,
        instance: &Instance,
    ) -> JobOutcome {
        let source = instance_data_dir(&self.data_root, &instance.uuid_short);
        let dest = self.store.archive_path(&instance.uuid_short, backup);

        ctx.progress("archiving", 0.0);
        let cancel = ctx.cancel().clone();
        let progress_fn = ctx.progress_fn();

        let result = tokio::task::spawn_blocking(move || {
            let is_cancelled = move || cancel.is_cancelled();
            // The archive engine takes plain closures; adapt the
            // context's reporters for the blocking thread.
            let report = move |fraction: f32| {
                if let Some(report) = &progress_fn {
                    report("archiving", fraction);
                }
            };
            crate::archive::create_archive(&source, &dest, &is_cancelled, &report)
        })
        .await
        .expect("archive task panicked");

        match result {
            Ok(archived) => {
                // Seal the row: bytes + sha256 + completed_at.
                let mut sealed = backup.clone();
                sealed.bytes = archived.bytes;
                sealed.sha256_hash = Some(archived.sha256);
                sealed.completed_at = Some(Utc::now());
                let persistence = Arc::clone(&self.persistence);
                let row = sealed.clone();
                if let Err(e) = tokio::task::spawn_blocking(move || persistence.put_backup(&row))
                    .await
                    .expect("backup seal task panicked")
                {
                    // The archive is fine; the row is not. Fail with
                    // cleanup: the archive without a row is the
                    // definition of partial state.
                    return JobOutcome::Failed {
                        message: format!("archive created but the row could not be sealed: {e}"),
                        cleanup_pending: true,
                    };
                }
                JobOutcome::Succeeded
            }
            Err(ArchiveError::Cancelled) => JobOutcome::Cancelled,
            Err(e) => JobOutcome::Failed {
                message: format!("backup archive failed: {e}"),
                // The .part file (if any) stays for the cleaner.
                cleanup_pending: true,
            },
        }
    }

    /// The restore direction (see the module docs).
    async fn run_restore(
        &self,
        ctx: JobContext,
        job: &Job,
        backup: &Backup,
        instance: &Instance,
    ) -> JobOutcome {
        let archive = self.store.archive_path(&instance.uuid_short, backup);
        let sha256 = backup.sha256_hash.clone().unwrap_or_default();
        let staging = self.staging_dir(&instance.uuid_short, &job.id.to_string());
        let live_dir = instance_data_dir(&self.data_root, &instance.uuid_short);

        // 1. Verify + extract into the staging directory.
        ctx.progress("verifying", 0.0);
        let cancel = ctx.cancel().clone();
        let progress_fn = ctx.progress_fn();
        let extract_staging = staging.clone();
        let result = tokio::task::spawn_blocking(move || {
            let is_cancelled = move || cancel.is_cancelled();
            let report = move |fraction: f32| {
                if let Some(report) = &progress_fn {
                    report("extracting", fraction);
                }
            };
            restore_archive(&archive, &sha256, &extract_staging, &is_cancelled, &report)
        })
        .await
        .expect("restore task panicked");

        match result {
            Err(ArchiveError::Cancelled) => return JobOutcome::Cancelled,
            Err(e) => {
                return JobOutcome::Failed {
                    message: format!("restore failed: {e}"),
                    // Staging (possibly partial) stays for the cleaner.
                    cleanup_pending: true,
                };
            }
            Ok(()) => {}
        }

        // 2. Swap the restored tree into the live directory in place
        //    (the directory itself is never renamed: a live bind
        //    mount pins it — see perform_in_place_swap).
        ctx.progress("swapping", 1.0);
        let swap = tokio::task::spawn_blocking(move || perform_in_place_swap(live_dir, staging))
            .await
            .expect("swap task panicked");
        match swap {
            Ok(()) => JobOutcome::Succeeded,
            Err(ArchiveError::Cancelled) => JobOutcome::Cancelled,
            Err(e) => JobOutcome::Failed {
                message: format!("restore swap failed: {e}"),
                cleanup_pending: true,
            },
        }
    }
}

/// Replace `live`'s *contents* with `staging`'s, in place.
///
/// The live directory itself is never renamed or removed: a running
/// container's bind mount pins that directory (rename leaves the
/// mount attached to the old inode — the container would keep
/// seeing the superseded data, and deleting the set-aside original
/// fails with EBUSY). Clearing the children and moving the restored
/// tree in keeps the mount valid, so the workload sees the restored
/// data immediately. A crash between the clear and the move-in
/// leaves a partially restored live tree — the archive is the
/// safety net; the job answers `cleanup_pending` and a retry
/// finishes the story.
fn perform_in_place_swap(live: PathBuf, staging: PathBuf) -> Result<(), ArchiveError> {
    let io = |context: &str, e: std::io::Error| ArchiveError::Io {
        context: context.to_owned(),
        source: e,
    };

    // 1. Clear the live contents (the directory itself stays).
    if live.exists() {
        for entry in std::fs::read_dir(&live).map_err(|e| io("cannot read the live data dir", e))? {
            let entry = entry.map_err(|e| io("cannot read the live data dir", e))?;
            let path = entry.path();
            let meta =
                std::fs::symlink_metadata(&path).map_err(|e| io("cannot stat live data", e))?;
            let removed = if meta.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            removed.map_err(|e| io("cannot clear the live data", e))?;
        }
    } else {
        std::fs::create_dir_all(&live).map_err(|e| io("cannot create the live data dir", e))?;
    }

    // 2. Move the restored tree in, entry by entry (same parent
    //    filesystem: renames are cheap and atomic per entry).
    for entry in
        std::fs::read_dir(&staging).map_err(|e| io("cannot read the restored staging dir", e))?
    {
        let entry = entry.map_err(|e| io("cannot read the restored staging dir", e))?;
        let name = entry.file_name();
        let dest = live.join(&name);
        if dest.exists() {
            // The clear above should have removed it; a concurrent
            // writer lost the race — the restore wins.
            let meta = std::fs::symlink_metadata(&dest)
                .map_err(|e| io("cannot stat colliding entry", e))?;
            if meta.is_dir() {
                std::fs::remove_dir_all(&dest)
            } else {
                std::fs::remove_file(&dest)
            }
            .map_err(|e| io("cannot clear colliding entry", e))?;
        }
        std::fs::rename(entry.path(), &dest)
            .map_err(|e| io("cannot move the restored data into place", e))?;
    }

    // 3. The staging directory is now empty.
    std::fs::remove_dir(&staging).map_err(|e| io("cannot remove the staging dir", e))?;
    Ok(())
}
