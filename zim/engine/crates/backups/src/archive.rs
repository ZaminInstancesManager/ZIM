//! The backup archive engine: create, verify, restore.
//!
//! Format: uncompressed `tar` inside gzip (`.tar.gz`), the Wings
//! backup format. Creation walks the instance's data directory
//! (regular files and directories; symlinks and specials are
//! skipped and reported), hashes the gzip stream while writing it
//! (one pass, one hash), and finalizes atomically: the archive is
//! written to `<uuid>.tar.gz.part` and renamed into place only after
//! every entry landed. A failure or cancellation leaves the `.part`
//! file behind — deliberately: the durable-jobs cleaner deletes it
//! (roadmap M8 acceptance: "the background cleaner deletes the
//! partial archive").
//!
//! Restore verifies the recorded sha256 before extraction and
//! rejects unsafe entries (absolute paths, `..` traversal, symlinks,
//! hard links, devices) so a hostile archive cannot escape its
//! target directory.

use std::fs;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Archive-engine failures.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// Cancellation was observed mid-archive; the partial `.part`
    /// output was left for the cleaner.
    #[error("archive operation cancelled")]
    Cancelled,
    /// An I/O error, with the operation context.
    #[error("{context}: {source}")]
    Io {
        /// What was happening.
        context: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The archive contains an entry that must not be extracted
    /// (escape attempt, symlink, ...).
    #[error("unsafe archive entry: {0}")]
    UnsafeEntry(String),
    /// The archive's sha256 does not match the recorded one.
    #[error("checksum mismatch: expected {expected}, computed {actual}")]
    ChecksumMismatch {
        /// The recorded digest.
        expected: String,
        /// The digest of the bytes on disk.
        actual: String,
    },
    /// The archive is not valid tar.gz (truncated, corrupt).
    #[error("corrupt archive: {0}")]
    Corrupt(String),
}

impl ArchiveError {
    fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

/// The outcome of a successful archive creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveResult {
    /// Size of the sealed archive in bytes.
    pub bytes: u64,
    /// Hex sha256 of the archive bytes.
    pub sha256: String,
}

/// A `Write` that hashes everything passing through it.
struct HashingWriter<W: Write> {
    inner: W,
    hasher: Sha256,
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.hasher.update(buf);
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// One walked file (for progress accounting).
struct WalkedFile {
    /// Absolute path.
    path: PathBuf,
    /// Path relative to the archive root (the stored name).
    name: String,
    /// Size in bytes.
    size: u64,
}

/// Recursively walk `source`, collecting regular files (and making
/// parent directories implicit). Symlinks and special files are
/// skipped — Minecraft data trees are files and directories; a
/// symlink in one is either an operator mistake or an escape
/// attempt, and neither belongs in an archive we will later restore
/// with privilege.
fn walk(source: &Path, is_cancelled: &dyn Fn() -> bool) -> Result<Vec<WalkedFile>, ArchiveError> {
    let mut files = Vec::new();
    let mut stack: Vec<PathBuf> = vec![source.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        let entries = fs::read_dir(&dir)
            .map_err(|e| ArchiveError::io(format!("cannot read dir {}", dir.display()), e))?;
        for entry in entries {
            let entry = entry
                .map_err(|e| ArchiveError::io(format!("cannot read dir {}", dir.display()), e))?;
            let path = entry.path();
            // lstat: do not follow symlinks out of the tree.
            let meta = fs::symlink_metadata(&path)
                .map_err(|e| ArchiveError::io(format!("cannot stat {}", path.display()), e))?;
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                let name = path
                    .strip_prefix(source)
                    .expect("walked path is under source")
                    .to_string_lossy()
                    .replace('\\', "/");
                files.push(WalkedFile {
                    path,
                    name,
                    size: meta.len(),
                });
            }
            // Symlinks / fifos / sockets / devices: skipped by
            // design (see the module docs).
        }
    }
    Ok(files)
}

/// Create a `.tar.gz` archive of `source`'s contents at `dest`.
///
/// The archive is assembled at `dest` + `.part` and atomically
/// renamed when complete. `is_cancelled` is polled between files
/// (and between directory reads); `progress` receives the
/// bytes-written fraction. On failure or cancellation the `.part`
/// file is deliberately left for the jobs cleaner.
pub fn create_archive(
    source: &Path,
    dest: &Path,
    is_cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(f32),
) -> Result<ArchiveResult, ArchiveError> {
    if !source.is_dir() {
        return Err(ArchiveError::io(
            format!("source directory {} does not exist", source.display()),
            std::io::Error::new(std::io::ErrorKind::NotFound, "no such directory"),
        ));
    }

    let part_path = part_path_for(dest);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| ArchiveError::io("cannot create archive directory", e))?;
    }

    let files = walk(source, is_cancelled)?;
    let total_bytes: u64 = files.iter().map(|f| f.size).sum::<u64>().max(1);
    let mut done_bytes: u64 = 0;
    progress(0.0);

    let part = File::create(&part_path)
        .map_err(|e| ArchiveError::io(format!("cannot create {}", part_path.display()), e))?;
    let writer = HashingWriter {
        inner: BufWriter::with_capacity(256 * 1024, part),
        hasher: Sha256::new(),
    };
    let gzip = flate2::write::GzEncoder::new(writer, flate2::Compression::default());
    let mut builder = tar::Builder::new(gzip);

    for file in &files {
        if is_cancelled() {
            // Drop the builder without finishing: the .part stays.
            return Err(ArchiveError::Cancelled);
        }
        append_file(&mut builder, file)
            .map_err(|e| ArchiveError::io(format!("cannot archive {}", file.path.display()), e))?;
        done_bytes += file.size;
        progress((done_bytes as f64 / total_bytes as f64) as f32);
    }

    // Finish tar → gzip → file, then fsync before the rename so a
    // crash after finalize never leaves a truncated "sealed" name.
    // `into_inner` writes the end-of-archive blocks and hands back
    // the gzip encoder; its `finish` flushes the compressed stream.
    let gzip = builder
        .into_inner()
        .map_err(|e| ArchiveError::io("cannot finish tar stream", e))?;
    let mut writer = gzip
        .finish()
        .map_err(|e| ArchiveError::io("cannot finish gzip stream", e))?;
    writer
        .flush()
        .map_err(|e| ArchiveError::io("cannot flush archive", e))?;
    let file = writer.inner.get_mut();
    file.sync_all()
        .map_err(|e| ArchiveError::io("cannot fsync archive", e))?;
    let sha256 = hex(&writer.hasher.clone().finalize());
    drop(writer);

    let bytes = fs::metadata(&part_path)
        .map_err(|e| ArchiveError::io("cannot stat sealed archive", e))?
        .len();
    fs::rename(&part_path, dest)
        .map_err(|e| ArchiveError::io("cannot finalize archive (rename)", e))?;
    progress(1.0);

    Ok(ArchiveResult { bytes, sha256 })
}

/// Append one file with an explicit header (mode/size/mtime from
/// lstat, so a swapped-in file mid-archive cannot lie about its
/// size).
fn append_file(builder: &mut tar::Builder<impl Write>, file: &WalkedFile) -> std::io::Result<()> {
    let meta = fs::symlink_metadata(&file.path)?;
    let mut f = File::open(&file.path)?;
    let mut header = tar::Header::new_gnu();
    header.set_size(meta.len());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        header.set_mode(meta.mode() & 0o777);
    }
    #[cfg(not(unix))]
    {
        header.set_mode(0o644);
    }
    header.set_mtime(
        meta.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0),
    );
    // Regular-file type is the header default for append_data.
    header.set_cksum();
    builder.append_data(&mut header, &file.name, &mut f)
}

/// `<path>.part` — the in-progress location.
pub fn part_path_for(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_os_string();
    name.push(".part");
    PathBuf::from(name)
}

/// Compute the sha256 of an existing file (streaming).
pub fn hash_file(path: &Path) -> Result<String, ArchiveError> {
    let f = File::open(path)
        .map_err(|e| ArchiveError::io(format!("cannot open {}", path.display()), e))?;
    let mut reader = BufReader::with_capacity(256 * 1024, f);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let read = reader
            .read(&mut buf)
            .map_err(|e| ArchiveError::io("cannot read archive", e))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

/// Verify an archive's bytes against `expected_sha256` (hex, case-
/// insensitive).
pub fn verify_archive(path: &Path, expected_sha256: &str) -> Result<(), ArchiveError> {
    let actual = hash_file(path)?;
    if actual.eq_ignore_ascii_case(expected_sha256) {
        Ok(())
    } else {
        Err(ArchiveError::ChecksumMismatch {
            expected: expected_sha256.to_owned(),
            actual,
        })
    }
}

/// Sum the entries' decompressed sizes (one streaming pass; the
/// iterator skips each entry's data as it goes).
fn scan_entry_bytes(archive: &Path) -> Result<u64, ArchiveError> {
    let f = File::open(archive)
        .map_err(|e| ArchiveError::io(format!("cannot open {}", archive.display()), e))?;
    let gzip = flate2::read::GzDecoder::new(BufReader::with_capacity(256 * 1024, f));
    let mut tar = tar::Archive::new(gzip);
    let mut total: u64 = 0;
    let iter = tar
        .entries()
        .map_err(|e| ArchiveError::Corrupt(format!("cannot read entries: {e}")))?;
    for entry in iter {
        total += entry
            .map_err(|e| ArchiveError::Corrupt(format!("cannot read entry: {e}")))?
            .size();
    }
    Ok(total)
}

/// Sanitize one stored entry name: it must be relative, contain no
/// `..` or `.` components, and contain no Windows drive prefix —
/// then it is safe to join onto the extraction root.
fn sanitize_entry(name: &str) -> Result<PathBuf, ArchiveError> {
    if name.is_empty() {
        return Err(ArchiveError::UnsafeEntry("empty entry name".to_owned()));
    }
    let path = Path::new(name);
    if path.is_absolute() {
        return Err(ArchiveError::UnsafeEntry(format!(
            "absolute entry path {name:?}"
        )));
    }
    for component in path.components() {
        match component {
            std::path::Component::Normal(_) => {}
            std::path::Component::CurDir => {
                return Err(ArchiveError::UnsafeEntry(format!(
                    "dot component in entry path {name:?}"
                )));
            }
            std::path::Component::ParentDir => {
                return Err(ArchiveError::UnsafeEntry(format!(
                    "parent-dir component in entry path {name:?}"
                )));
            }
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                return Err(ArchiveError::UnsafeEntry(format!(
                    "absolute or prefixed entry path {name:?}"
                )));
            }
        }
    }
    Ok(path.to_path_buf())
}

/// Extract a verified archive into `target_dir` (which must exist —
/// or be created by the caller as the staging directory).
///
/// Only directories and regular files are extracted; every entry is
/// sanitized against escapes first. `expected_sha256` is verified
/// before any extraction begins. `is_cancelled` is polled between
/// entries; a cancelled extraction leaves partial output in
/// `target_dir` for the caller's cleanup story (the caller extracts
/// into a staging directory, so the live data is never half-written).
///
/// Two streaming passes over the archive: one to size the work
/// (progress denominator), one to extract. Entries cannot be
/// collected up-front — the tar iterator advances past each entry's
/// data to yield the next header, so a retained `Entry`'s data
/// window is stale by the time it is unpacked.
pub fn restore_archive(
    archive: &Path,
    expected_sha256: &str,
    target_dir: &Path,
    is_cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(f32),
) -> Result<(), ArchiveError> {
    verify_archive(archive, expected_sha256)?;

    fs::create_dir_all(target_dir)
        .map_err(|e| ArchiveError::io("cannot create extraction directory", e))?;

    // Pass 1: total decompressed entry bytes (the progress
    // denominator).
    let total = scan_entry_bytes(archive)?.max(1);

    // Pass 2: extract, entry by entry.
    let f = File::open(archive)
        .map_err(|e| ArchiveError::io(format!("cannot open {}", archive.display()), e))?;
    let gzip = flate2::read::GzDecoder::new(BufReader::with_capacity(256 * 1024, f));
    let mut tar = tar::Archive::new(gzip);

    let mut done: u64 = 0;
    progress(0.0);
    let iter = tar
        .entries()
        .map_err(|e| ArchiveError::Corrupt(format!("cannot read entries: {e}")))?;
    for entry in iter {
        if is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        let mut entry =
            entry.map_err(|e| ArchiveError::Corrupt(format!("cannot read entry: {e}")))?;
        let name = entry
            .path()
            .map_err(|e| ArchiveError::Corrupt(format!("unreadable entry path: {e}")))?
            .to_string_lossy()
            .replace('\\', "/");
        let safe = sanitize_entry(&name)?;
        let dest = target_dir.join(&safe);

        match entry.header().entry_type() {
            t if t.is_dir() => {
                fs::create_dir_all(&dest).map_err(|e| {
                    ArchiveError::io(format!("cannot create {}", dest.display()), e)
                })?;
            }
            t if t.is_file() => {
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent).map_err(|e| {
                        ArchiveError::io(format!("cannot create {}", parent.display()), e)
                    })?;
                }
                entry.unpack(&dest).map_err(|e| {
                    ArchiveError::io(format!("cannot extract {}", dest.display()), e)
                })?;
            }
            t => {
                return Err(ArchiveError::UnsafeEntry(format!(
                    "entry {name:?} has unsupported type {t:?} (symlink/link/device)"
                )));
            }
        }
        done += entry.size();
        progress((done as f64 / total as f64) as f32);
    }
    progress(1.0);
    Ok(())
}

/// Replace `live`'s *contents* with `staging`'s, in place.
///
/// The live directory itself is never renamed or removed: a running
/// container's bind mount pins that directory (rename leaves the
/// mount attached to the old inode — the container would keep seeing
/// the superseded data, and deleting the set-aside original fails
/// with EBUSY). Clearing the children and moving the staged tree in
/// keeps the mount valid, so the workload sees the new data
/// immediately. A crash between the clear and the move-in leaves a
/// partially replaced live tree — the archive the staged tree came
/// from is the safety net; the caller's job answers
/// `cleanup_pending` and a retry finishes the story.
///
/// Shared by the backup restore direction and the transfer extract
/// phase (M8.2): both stage a verified tree and commit it under the
/// live directory.
pub fn swap_contents_in_place(live: &Path, staging: &Path) -> Result<(), ArchiveError> {
    let io = |context: &str, e: std::io::Error| ArchiveError::Io {
        context: context.to_owned(),
        source: e,
    };

    // 1. Clear the live contents (the directory itself stays).
    if live.exists() {
        for entry in fs::read_dir(live).map_err(|e| io("cannot read the live data dir", e))? {
            let entry = entry.map_err(|e| io("cannot read the live data dir", e))?;
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).map_err(|e| io("cannot stat live data", e))?;
            let removed = if meta.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
            removed.map_err(|e| io("cannot clear the live data", e))?;
        }
    } else {
        fs::create_dir_all(live).map_err(|e| io("cannot create the live data dir", e))?;
    }

    // 2. Move the staged tree in, entry by entry (same parent
    //    filesystem: renames are cheap and atomic per entry).
    for entry in fs::read_dir(staging).map_err(|e| io("cannot read the staged dir", e))? {
        let entry = entry.map_err(|e| io("cannot read the staged dir", e))?;
        let name = entry.file_name();
        let dest = live.join(&name);
        if dest.exists() {
            // The clear above should have removed it; a concurrent
            // writer lost the race — the staged tree wins.
            let meta =
                fs::symlink_metadata(&dest).map_err(|e| io("cannot stat colliding entry", e))?;
            if meta.is_dir() {
                fs::remove_dir_all(&dest)
            } else {
                fs::remove_file(&dest)
            }
            .map_err(|e| io("cannot clear colliding entry", e))?;
        }
        fs::rename(entry.path(), &dest)
            .map_err(|e| io("cannot move the staged data into place", e))?;
    }

    // 3. The staging directory is now empty.
    fs::remove_dir(staging).map_err(|e| io("cannot remove the staging dir", e))?;
    Ok(())
}

/// Lowercase hex of a digest.
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}
