//! The on-disk layout of backup archives.
//!
//! ```text
//! {data_root}/backups/{instance_uuid_short}/{backup_uuid}.tar.gz
//! {data_root}/backups/{instance_uuid_short}/{backup_uuid}.tar.gz.part
//! ```
//!
//! The `.part` sibling exists only while an archive is being
//! written (or after a failed/interrupted one — the cleaner removes
//! it). Archives are grouped per instance so pruning an instance's
//! backups (or the whole instance) is one directory.

use std::path::{Path, PathBuf};

use zim_engine_domain::Backup;

/// Where backup archives live under the engine's data root.
#[derive(Debug, Clone)]
pub struct ArchiveStore {
    root: PathBuf,
}

impl ArchiveStore {
    /// `<data_root>/backups`.
    pub fn new(data_root: &Path) -> Self {
        Self {
            root: data_root.join("backups"),
        }
    }

    /// The store root (observability / tests).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The per-instance archive directory.
    pub fn instance_dir(&self, instance_uuid_short: &str) -> PathBuf {
        self.root.join(instance_uuid_short)
    }

    /// The sealed archive path for a backup.
    pub fn archive_path(&self, instance_uuid_short: &str, backup: &Backup) -> PathBuf {
        self.instance_dir(instance_uuid_short)
            .join(format!("{}.tar.gz", backup.uuid))
    }

    /// The in-progress (or leftover) partial archive path.
    pub fn part_path(&self, instance_uuid_short: &str, backup: &Backup) -> PathBuf {
        crate::archive::part_path_for(&self.archive_path(instance_uuid_short, backup))
    }

    /// Remove every archive (and partial) for an instance — used by
    /// the handler's cleanup when the instance itself is gone.
    pub fn remove_instance_dir(&self, instance_uuid_short: &str) -> std::io::Result<()> {
        let dir = self.instance_dir(instance_uuid_short);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}
