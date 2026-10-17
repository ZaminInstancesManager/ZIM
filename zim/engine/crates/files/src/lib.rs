//! File API (M17) — list, read, write, delete, archive, extract.
//!
//! The original Wings file API reproduced in Rust: a
//! [`Filesystem`] handle is rooted at one instance's data directory
//! and every operation is path-validated against that root. Path
//! traversal and symlink attacks are blocked per
//! `docs/security/THREAT_MODEL.md` §2.6 (see [`path`]); archive
//! extraction has configurable max uncompressed size, entry count,
//! and nesting depth per §2.7 (see [`archive`]).
//!
//! # Wings parity notes
//!
//! - `list_dir`/`read_file`/`write_file`/`delete` mirror Wings's
//!   `Filesystem` surface; `write_file` is atomic
//!   (write-to-temp-then-rename) so a crashed writer never leaves
//!   a torn file where Wings could.
//! - `rename` mirrors Wings's `Rename` (M28): one §2.6 validation
//!   on each end, root containment for the move as a whole, and a
//!   pre-syscall refusal for directory-into-itself moves.
//! - `archive` is Wings's `CompressFiles`: an allow-list of paths
//!   inside the root compressed into one archive (tar/tar.gz/zip);
//!   `.tar.gz` is the format Wings's own compressor emits.
//! - `extract` is Wings's `DecompressFile`-family behavior
//!   (decompress-where-it-stands) with the §2.7 caps enforced.
//!
//! # Architecture boundary
//!
//! This crate's role, dependencies, and permitted callers are documented
//! in `docs/architecture/RUST_ENGINE.md` (and the expanded-scope
//! `docs/architecture/WORKLOAD_ABSTRACTION.md` where applicable).
//! The permitted dependency direction is enforced at compile time by
//! the workspace crate graph. The crate is deliberately
//! self-contained (no engine-internal dependencies): the file API
//! is a leaf capability the workload integrations
//! (`zim_engine_workloads_minecraft`, M17) compose with their
//! runtime handles.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod archive;
pub mod path;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

pub use archive::{
    archive, extract, extract_with_limits, ArchiveFormat, ExtractLimits, ExtractStats,
};
pub use path::SafePath;

/// The file API's error vocabulary.
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
pub enum FileError {
    /// The requested path was empty or normalized to nothing.
    #[error("the requested path is empty")]
    EmptyPath,
    /// The requested path escapes the data directory (§2.6's
    /// resolution rule: a `..` sequence or an escaping symlink).
    #[error("path escapes the data directory: {0:?}")]
    Traversal(String),
    /// The requested path was absolute; only data-root-relative
    /// paths are accepted.
    #[error("absolute paths are rejected: {0:?}")]
    Absolute(String),
    /// A path that resolved through symlinks to somewhere outside
    /// the data directory.
    #[error("symlink target escapes the data directory: {0:?}")]
    EscapesRoot(String),
    /// The path does not exist.
    #[error("not found: {0:?}")]
    NotFound(String),
    /// The path exists but is the wrong kind for the operation
    /// (a directory where a file was required, or vice versa).
    #[error("wrong file kind at {0:?}")]
    NotAFile(String),
    /// The operation exceeded a configured limit (§2.7 caps: size,
    /// entry count, nesting depth).
    #[error("limit exceeded: {0}")]
    Limit(String),
    /// The archive contained a path an earlier entry already
    /// created (§2.7's overlap rule).
    #[error("overlapping archive entry: {0:?}")]
    Overlap(String),
    /// The archive contained a symlink/hard-link entry (rejected
    /// per the §2.7 tightening).
    #[error("link entry in archive is rejected: {0:?}")]
    LinkEntry(String),
    /// The archive's format is not one the file API reads.
    #[error("unsupported archive format: {0:?}")]
    UnsupportedFormat(String),
    /// An underlying IO error, with the operation's context.
    #[error("{context}: {source}")]
    Io {
        /// What was being attempted.
        context: String,
        /// The IO error itself.
        source: std::io::Error,
    },
}

impl FileError {
    /// The constructor the crate's internals use for IO mapping.
    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    /// Whether this is a not-found-shaped error (the API layer maps
    /// it onto 404 responses; other shapes are 400-class).
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound(_))
    }
}

/// One listed directory entry (Wings's stat shape, trimmed to the
/// portable fields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// The entry's name (not the full path).
    pub name: String,
    /// Whether the entry is a directory.
    pub is_dir: bool,
    /// The entry's size in bytes (0 for directories).
    pub size: u64,
    /// The entry's last-modified time.
    pub modified: Option<DateTime<Utc>>,
}

/// A path-validated file API rooted at one instance's data
/// directory.
///
/// Created per instance by the workload layer (M17:
/// `zim_engine_workloads_minecraft`); cheap to clone (an `Arc`
/// around the shared root + limits).
#[derive(Debug, Clone)]
pub struct Filesystem {
    inner: std::sync::Arc<Inner>,
}

#[derive(Debug, Clone)]
struct Inner {
    /// The data directory (as configured; resolved canonically per
    /// operation by [`SafePath::resolve`]).
    root: PathBuf,
    /// The §2.7 extraction limits.
    limits: ExtractLimits,
}

impl Filesystem {
    /// A filesystem rooted at `root` with the default extraction
    /// limits.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            inner: std::sync::Arc::new(Inner {
                root: root.into(),
                limits: ExtractLimits::default(),
            }),
        }
    }

    /// Override the extraction limits (§2.7's "configurable").
    pub fn with_limits(mut self, limits: ExtractLimits) -> Self {
        let inner = std::sync::Arc::make_mut(&mut self.inner);
        inner.limits = limits;
        self
    }

    /// The configured root.
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// The configured extraction limits.
    pub fn limits(&self) -> &ExtractLimits {
        &self.inner.limits
    }

    /// Resolve a root-relative request path (§2.6).
    pub fn resolve(&self, path: &str) -> Result<SafePath, FileError> {
        SafePath::resolve(&self.inner.root, path)
    }

    /// List one directory's entries (sorted by name).
    ///
    /// The root itself may be listed by requesting `"."` — the one
    /// place the empty relative form is meaningful.
    pub fn list_dir(&self, path: &str) -> Result<Vec<DirEntry>, FileError> {
        let entries: Vec<DirEntry> = if path.trim() == "." || path.trim().is_empty() {
            // The root's own listing: no SafePath needed (it is the
            // root by construction).
            let canonical = self
                .inner
                .root
                .canonicalize()
                .map_err(|e| FileError::io("cannot resolve data root", e))?;
            read_dir_entries(&canonical)?
        } else {
            let safe = self.resolve(path)?;
            let meta = fs::metadata(safe.canonical()).map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    FileError::NotFound(safe.relative().to_owned())
                } else {
                    FileError::io("cannot stat directory", e)
                }
            })?;
            if !meta.is_dir() {
                return Err(FileError::NotAFile(safe.relative().to_owned()));
            }
            read_dir_entries(safe.canonical())?
        };
        Ok(entries)
    }

    /// Read one file's bytes.
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, FileError> {
        let safe = self.resolve(path)?;
        let meta = fs::metadata(safe.canonical()).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                FileError::NotFound(safe.relative().to_owned())
            } else {
                FileError::io("cannot stat file", e)
            }
        })?;
        if !meta.is_file() {
            return Err(FileError::NotAFile(safe.relative().to_owned()));
        }
        fs::read(safe.canonical()).map_err(|e| FileError::io("cannot read file", e))
    }

    /// Write one file's bytes atomically: the content lands in a
    /// sibling temp file, then renames over the target. A crashed
    /// writer leaves either the old or the new content, never a
    /// torn mix (a documented tightening over Wings's direct
    /// `WriteFile`).
    ///
    /// Parent directories are created as needed (Wings's
    /// `WriteFile` semantics for nested paths).
    pub fn write_file(&self, path: &str, content: &[u8]) -> Result<(), FileError> {
        let safe = self.resolve(path)?;
        let target = safe.canonical();
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| FileError::io("cannot create parent directory", e))?;
        }
        let file_name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_owned());
        // A dot-prefixed sibling keeps the temp file inside the same
        // directory (same filesystem → the rename stays atomic) and
        // out of ordinary listings' way.
        let temp = target
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!(".{file_name}.zimtmp"));
        {
            let mut out = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&temp)
                .map_err(|e| FileError::io("cannot create temp file", e))?;
            out.write_all(content)
                .map_err(|e| FileError::io("cannot write temp file", e))?;
            out.sync_all()
                .map_err(|e| FileError::io("cannot sync temp file", e))?;
        }
        fs::rename(&temp, target).map_err(|e| {
            let _ = fs::remove_file(&temp);
            FileError::io("cannot replace file", e)
        })
    }

    /// Delete one file or directory (directories recursively), the
    /// §2.6-validated path only.
    pub fn delete(&self, path: &str) -> Result<(), FileError> {
        let safe = self.resolve(path)?;
        let target = safe.canonical();
        let meta = match fs::symlink_metadata(target) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(FileError::NotFound(safe.relative().to_owned()));
            }
            Err(e) => return Err(FileError::io("cannot stat for delete", e)),
        };
        // The canonical path is inside the root (SafePath proved
        // it); a trailing symlink at the leaf deletes the link
        // itself, not its target — symlink_metadata above is what
        // keeps that true.
        if meta.is_dir() {
            fs::remove_dir_all(target).map_err(|e| FileError::io("cannot remove directory", e))
        } else {
            fs::remove_file(target).map_err(|e| FileError::io("cannot remove file", e))
        }
    }

    /// Rename or move one path inside the root (Wings's `Rename`):
    /// both ends are §2.6-validated first, and the rename refuses to
    /// cross out of the root even when both ends individually resolve
    /// inside it (a move is one operation, not two).
    ///
    /// Renaming onto an existing path follows `std::fs::rename`
    /// semantics (POSIX replaces files atomically; on Windows an
    /// existing destination file is replaced after a best-effort
    /// remove, directories are never replaced).
    pub fn rename(&self, old_path: &str, new_path: &str) -> Result<(), FileError> {
        let from = self.resolve(old_path)?;
        let to = self.resolve(new_path)?;
        // symlink_metadata (not metadata) so a broken symlink at the
        // source is still a renameable object, not a phantom.
        if std::fs::symlink_metadata(from.canonical()).is_err() {
            return Err(FileError::NotFound(old_path.to_owned()));
        }
        // Destination parents appear as needed — the same semantics
        // write_file gives nested paths (a move is a write).
        if let Some(parent) = to.canonical().parent() {
            fs::create_dir_all(parent)
                .map_err(|e| FileError::io("cannot create destination parent", e))?;
        }
        // A directory renamed into its own subtree would detach it
        // from the root's tree (POSIX: EINVAL). Guard it before the
        // syscall so the error is a validation, not an IO accident.
        if std::fs::metadata(from.canonical())
            .map(|m| m.is_dir())
            .unwrap_or(false)
            && to.canonical().starts_with(from.canonical())
        {
            return Err(FileError::Limit(format!(
                "cannot move a directory inside itself: {old_path:?} -> {new_path:?}"
            )));
        }
        // Windows cannot atomically replace an existing destination
        // file through std::fs::rename; remove it first (the source
        // resolve above already proved the operation stays inside the
        // root). Directories are left to the syscall so a non-empty
        // destination fails honestly.
        #[cfg(windows)]
        if let Ok(meta) = std::fs::symlink_metadata(to.canonical()) {
            if meta.is_file() {
                std::fs::remove_file(to.canonical())
                    .map_err(|e| FileError::io("cannot replace destination", e))?;
            }
        }
        std::fs::rename(from.canonical(), to.canonical())
            .map_err(|e| FileError::io("cannot rename", e))
    }

    /// Create one directory (with parents), §2.6-validated.
    pub fn mkdir(&self, path: &str) -> Result<(), FileError> {
        let safe = self.resolve(path)?;
        fs::create_dir_all(safe.canonical())
            .map_err(|e| FileError::io("cannot create directory", e))
    }

    /// Stat one path (file or directory).
    pub fn stat(&self, path: &str) -> Result<DirEntry, FileError> {
        if path.trim() == "." || path.trim().is_empty() {
            let canonical = self
                .inner
                .root
                .canonicalize()
                .map_err(|e| FileError::io("cannot resolve data root", e))?;
            return Ok(DirEntry {
                name: String::new(),
                is_dir: true,
                size: 0,
                modified: modified_of(&canonical),
            });
        }
        let safe = self.resolve(path)?;
        let meta = fs::metadata(safe.canonical()).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                FileError::NotFound(safe.relative().to_owned())
            } else {
                FileError::io("cannot stat", e)
            }
        })?;
        Ok(DirEntry {
            name: safe
                .relative()
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_owned(),
            is_dir: meta.is_dir(),
            size: if meta.is_dir() { 0 } else { meta.len() },
            modified: modified_of(safe.canonical()),
        })
    }

    /// Compress an allow-list of paths into one archive inside the
    /// root (Wings's `CompressFiles`); see [`archive::archive`].
    pub fn archive(
        &self,
        dest: &str,
        paths: &[&str],
        format: ArchiveFormat,
    ) -> Result<u64, FileError> {
        archive::archive(&self.inner.root, dest, paths, format)
    }

    /// Extract an archive under a destination inside the root, with
    /// this handle's limits; see [`archive::extract_with_limits`].
    pub fn extract(&self, archive_path: &str, dest: &str) -> Result<ExtractStats, FileError> {
        extract_with_limits(&self.inner.root, archive_path, dest, &self.inner.limits)
    }
}

/// Read one directory's entries, sorted by name.
fn read_dir_entries(dir: &Path) -> Result<Vec<DirEntry>, FileError> {
    let mut out = Vec::new();
    let entries = fs::read_dir(dir).map_err(|e| FileError::io("cannot list directory", e))?;
    for entry in entries {
        let entry = entry.map_err(|e| FileError::io("cannot read directory entry", e))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // Follow symlinks for the listing's shape (Wings's stat
        // does); an escaping symlink's *target* still cannot be
        // opened through this API — every read/write resolves
        // through SafePath first.
        let meta = entry
            .metadata()
            .map_err(|e| FileError::io("cannot stat entry", e))?;
        out.push(DirEntry {
            name,
            is_dir: meta.is_dir(),
            size: if meta.is_dir() { 0 } else { meta.len() },
            modified: modified_of(&entry.path()),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// One path's mtime, when available.
fn modified_of(path: &Path) -> Option<DateTime<Utc>> {
    fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .map(DateTime::<Utc>::from)
}

/// The canonical crate name.
pub const CRATE_NAME: &str = "zim_engine_files";

#[cfg(test)]
mod tests {
    //! The handle-level unit surface; the attack-path coverage
    //! (traversal, symlinks, archive bombs, zip-slip) lives in
    //! `tests/files.rs` where a real tree is arranged.

    use super::*;

    fn tmp_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!("zim-files-lib-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create temp root");
        root
    }

    #[test]
    fn crate_name_is_set() {
        assert_eq!(CRATE_NAME, "zim_engine_files");
    }

    #[test]
    fn default_limits_match_the_threat_model() {
        let limits = ExtractLimits::default();
        assert_eq!(limits.max_uncompressed_bytes, 5 * 1024 * 1024 * 1024);
        assert_eq!(limits.max_entries, 100_000);
        assert_eq!(limits.max_nesting_depth, 3);
    }

    #[test]
    fn write_read_delete_round_trip() {
        let root = tmp_root();
        let fs_api = Filesystem::new(&root);
        fs_api.write_file("eula.txt", b"eula=true\n").unwrap();
        assert_eq!(fs_api.read_file("eula.txt").unwrap(), b"eula=true\n");
        fs_api.delete("eula.txt").unwrap();
        assert!(matches!(
            fs_api.read_file("eula.txt"),
            Err(FileError::NotFound(_))
        ));
    }

    #[test]
    fn write_creates_parent_directories() {
        let root = tmp_root();
        let fs_api = Filesystem::new(&root);
        fs_api.write_file("worlds/world/level.dat", b"dat").unwrap();
        assert_eq!(fs_api.read_file("worlds/world/level.dat").unwrap(), b"dat");
    }

    #[test]
    fn no_temp_file_survives_a_successful_write() {
        let root = tmp_root();
        let fs_api = Filesystem::new(&root);
        fs_api
            .write_file("server.properties", b"server-port=25565\n")
            .unwrap();
        let listing = fs_api.list_dir(".").unwrap();
        assert_eq!(listing.len(), 1);
        assert_eq!(listing[0].name, "server.properties");
    }

    #[test]
    fn list_dir_is_sorted_and_reports_shapes() {
        let root = tmp_root();
        let fs_api = Filesystem::new(&root);
        fs_api.mkdir("plugins").unwrap();
        fs_api.write_file("b.txt", b"bb").unwrap();
        fs_api.write_file("a.txt", b"a").unwrap();
        let names: Vec<(String, bool, u64)> = fs_api
            .list_dir(".")
            .unwrap()
            .into_iter()
            .map(|e| (e.name, e.is_dir, e.size))
            .collect();
        assert_eq!(
            names,
            vec![
                ("a.txt".to_owned(), false, 1),
                ("b.txt".to_owned(), false, 2),
                ("plugins".to_owned(), true, 0),
            ]
        );
    }

    #[test]
    fn stat_reports_file_shapes() {
        let root = tmp_root();
        let fs_api = Filesystem::new(&root);
        fs_api.write_file("a.txt", b"abc").unwrap();
        let stat = fs_api.stat("a.txt").unwrap();
        assert_eq!(stat.name, "a.txt");
        assert!(!stat.is_dir);
        assert_eq!(stat.size, 3);
        assert!(stat.modified.is_some());
        let stat = fs_api.stat(".").unwrap();
        assert!(stat.is_dir);
    }

    #[test]
    fn not_found_and_not_a_file_are_distinguished() {
        let root = tmp_root();
        let fs_api = Filesystem::new(&root);
        assert!(fs_api.read_file("missing.txt").unwrap_err().is_not_found());
        fs_api.mkdir("adir").unwrap();
        assert!(matches!(
            fs_api.read_file("adir"),
            Err(FileError::NotAFile(_))
        ));
        assert!(matches!(
            fs_api.list_dir("missing"),
            Err(FileError::NotFound(_))
        ));
        assert!(matches!(
            fs_api.list_dir("adir/file.txt"),
            Err(FileError::NotFound(_))
        ));
    }

    #[test]
    fn the_root_listing_and_stat_do_not_require_a_safe_path() {
        let root = tmp_root();
        let fs_api = Filesystem::new(&root);
        fs_api.write_file("x.txt", b"x").unwrap();
        assert_eq!(fs_api.list_dir("").unwrap().len(), 1);
        assert!(fs_api.stat(".").unwrap().is_dir);
    }

    #[test]
    fn with_limits_overrides_the_extraction_caps() {
        let root = tmp_root();
        let fs_api = Filesystem::new(&root).with_limits(ExtractLimits {
            max_uncompressed_bytes: 10,
            max_entries: 2,
            max_nesting_depth: 1,
        });
        assert_eq!(fs_api.limits().max_uncompressed_bytes, 10);
        assert_eq!(fs_api.limits().max_entries, 2);
        assert_eq!(fs_api.limits().max_nesting_depth, 1);
        // Arc::make_mut keeps clones independent.
        let other = fs_api.clone();
        let fs_api = other.with_limits(ExtractLimits::default());
        assert_eq!(fs_api.limits().max_entries, 100_000);
    }

    #[test]
    fn rename_moves_a_file_and_creates_parents_at_the_destination() {
        let root = tmp_root();
        let fs_api = Filesystem::new(&root);
        fs_api
            .write_file("server.properties", b"port=25565")
            .unwrap();
        fs_api
            .rename("server.properties", "backup/server.properties")
            .unwrap();
        assert_eq!(
            fs_api.read_file("backup/server.properties").unwrap(),
            b"port=25565"
        );
        assert!(fs_api.read_file("server.properties").is_err());
    }

    #[test]
    fn rename_refuses_missing_sources_and_self_moves() {
        let root = tmp_root();
        let fs_api = Filesystem::new(&root);
        assert!(matches!(
            fs_api.rename("ghost.txt", "moved.txt"),
            Err(FileError::NotFound(_))
        ));
        fs_api.mkdir("world").unwrap();
        fs_api.write_file("world/level.dat", b"dat").unwrap();
        // A directory cannot move inside itself.
        assert!(matches!(
            fs_api.rename("world", "world/nested"),
            Err(FileError::Limit(_))
        ));
        // The tree is untouched by the refusals.
        assert_eq!(fs_api.list_dir("world").unwrap().len(), 1);
    }

    #[test]
    fn rename_validates_both_ends_against_the_root() {
        let root = tmp_root();
        let fs_api = Filesystem::new(&root);
        fs_api.write_file("a.txt", b"a").unwrap();
        assert!(matches!(
            fs_api.rename("a.txt", "../escape.txt"),
            Err(FileError::Traversal(_))
        ));
        assert!(matches!(
            fs_api.rename("../outside.txt", "b.txt"),
            Err(FileError::Traversal(_))
        ));
        assert!(matches!(
            fs_api.rename("a.txt", "/etc/passwd"),
            Err(FileError::Absolute(_))
        ));
        // Nothing escaped: the file is still where it was.
        assert_eq!(fs_api.read_file("a.txt").unwrap(), b"a");
    }
}
