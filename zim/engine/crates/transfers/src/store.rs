//! The on-disk layout of a transfer's working tree.
//!
//! Every transfer owns a directory under the engine's data root:
//!
//! ```text
//! {data_root}/transfers/{instance_short}/{transfer_id}/
//! ├── source/ archive.tar.gz        # created here (atomic .part → rename)
//! ├── dest/   archive.tar.gz        # moved here by the "ship" step
//! └── staging/                      # digest-verified extraction target
//! ```
//!
//! The `source/` half is what the source node writes; the `dest/`
//! half is what the destination node reads. In this milestone both
//! live on the engine's filesystem and the shipping step is a
//! rename plus a digest verification — when the node-runtime
//! contract lands, the bytes cross the wire into `dest/` instead,
//! and nothing else about the layout or the failure story changes.
//!
//! The whole per-transfer directory is removable at any point: it
//! is derived state (the instance's live directory is only ever
//! replaced from a *verified* archive), so the job cleaner deletes
//! it for failed transfers and the handler deletes it on success.

use std::path::{Path, PathBuf};

/// The archive file name inside a transfer's halves.
const ARCHIVE_NAME: &str = "archive.tar.gz";

/// Resolver for a transfer's on-disk tree (see the module docs).
#[derive(Debug, Clone)]
pub struct TransferStore {
    data_root: PathBuf,
}

impl TransferStore {
    /// Build the store over the engine's data root.
    pub fn new(data_root: &Path) -> Self {
        Self {
            data_root: data_root.to_path_buf(),
        }
    }

    /// The per-transfer working directory.
    pub fn transfer_dir(&self, instance_short: &str, transfer_id: &str) -> PathBuf {
        self.data_root
            .join("transfers")
            .join(instance_short)
            .join(transfer_id)
    }

    /// Where the source-side archive is assembled (and finalized).
    pub fn source_archive(&self, instance_short: &str, transfer_id: &str) -> PathBuf {
        self.transfer_dir(instance_short, transfer_id)
            .join("source")
            .join(ARCHIVE_NAME)
    }

    /// Where the destination-side archive lands after shipping.
    pub fn dest_archive(&self, instance_short: &str, transfer_id: &str) -> PathBuf {
        self.transfer_dir(instance_short, transfer_id)
            .join("dest")
            .join(ARCHIVE_NAME)
    }

    /// The destination-side extraction staging directory.
    pub fn staging_dir(&self, instance_short: &str, transfer_id: &str) -> PathBuf {
        self.transfer_dir(instance_short, transfer_id)
            .join("staging")
    }

    /// Remove the whole per-transfer tree. "Nothing there" is
    /// success (the cleaner's contract: idempotent, and no partial
    /// state left behind is the goal, not an error).
    pub fn remove_transfer_tree(
        &self,
        instance_short: &str,
        transfer_id: &str,
    ) -> Result<(), String> {
        let dir = self.transfer_dir(instance_short, transfer_id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .map_err(|e| format!("cannot remove {}: {e}", dir.display()))?;
        }
        Ok(())
    }

    /// Remove the per-instance transfers directory when empty (a
    /// successful transfer leaves it that way; failing to prune it
    /// is cosmetic, not an error).
    pub fn prune_instance_dir(&self, instance_short: &str) {
        let dir = self.data_root.join("transfers").join(instance_short);
        if dir
            .read_dir()
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false)
        {
            let _ = std::fs::remove_dir(&dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zim-transfer-store-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn paths_are_keyed_by_instance_and_transfer() {
        let store = TransferStore::new(Path::new("/data"));
        let dir = store.transfer_dir("abcd1234", "t-1");
        assert_eq!(dir, Path::new("/data/transfers/abcd1234/t-1"));
        assert_eq!(
            store.source_archive("abcd1234", "t-1"),
            Path::new("/data/transfers/abcd1234/t-1/source/archive.tar.gz")
        );
        assert_eq!(
            store.dest_archive("abcd1234", "t-1"),
            Path::new("/data/transfers/abcd1234/t-1/dest/archive.tar.gz")
        );
        assert_eq!(
            store.staging_dir("abcd1234", "t-1"),
            Path::new("/data/transfers/abcd1234/t-1/staging")
        );
    }

    #[test]
    fn removing_a_missing_tree_is_success() {
        let root = scratch("remove-missing");
        let store = TransferStore::new(&root);
        store
            .remove_transfer_tree("nope", "nope")
            .expect("nothing to remove is success");
    }

    #[test]
    fn removing_a_live_tree_deletes_all_halves() {
        let root = scratch("remove-live");
        let store = TransferStore::new(&root);
        for path in [store.source_archive("s", "t"), store.staging_dir("s", "t")] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"x").unwrap();
        }
        store.remove_transfer_tree("s", "t").unwrap();
        assert!(!store.transfer_dir("s", "t").exists());
    }
}
