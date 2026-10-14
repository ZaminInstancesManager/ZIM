//! Archive assembly and extraction (M17, the Wings file-API surface).
//!
//! `archive` compresses an allow-list of paths into a `.tar.gz`,
//! `.tar`, or `.zip` (Wings's `CompressFiles` semantics: nested
//! paths select files/folders inside a larger directory; the
//! output lands next to them). Entries are emitted in sorted order
//! so identical trees produce byte-identical archives.
//!
//! `extract` unpacks an archive under a destination directory with
//! the §2.7 defenses:
//!
//! - **traversal-aware entry names** — `..` components, absolute
//!   paths, and backslash separators are rejected before any file
//!   is created (the same lexical rules [`SafePath`][crate::SafePath]
//!   applies);
//! - **entry-kind allow-list** — only regular files and directories
//!   are unpacked; symlink and hard-link entries are rejected
//!   outright (a documented tightening of §2.7's "symlink entries
//!   whose targets are outside the extraction directory are
//!   rejected" — a link's target cannot be known safely before it
//!   exists);
//! - **overlapping entries** — two entries mapping to the same path
//!   within one extraction are rejected instead of silently
//!   overwriting;
//! - **bounded work** — cumulative uncompressed bytes and entry
//!   count are capped ([`ExtractLimits`]); a zip-bomb-style ratio
//!   fails fast before the copy begins (the declared size is
//!   charged up front);
//! - **nested archives** — an archive entry that is itself an
//!   archive is extracted recursively, bounded by the nesting-depth
//!   cap (default 3, §2.7's "archives containing archives"). At the
//!   cap the nested archive is left as a plain file rather than an
//!   error: the bytes are already validated and stored, only the
//!   recursion stops.
//!
//! The §2.7 seccomp-constrained extraction subprocess is a
//! Linux-specific defense-in-depth item and stays deferred; the
//! byte/entry/depth caps above are the portably enforceable subset
//! and are what this milestone's tests exercise.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use flate2::write::GzEncoder;
use flate2::Compression;
use tar::{Builder, EntryType, Header};

use crate::FileError;

/// The archive formats the file API writes and reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    /// Uncompressed tar.
    Tar,
    /// gzip-compressed tar (Wings's default backup/archive shape).
    TarGz,
    /// Zip (deflate; the stored form is read too).
    Zip,
}

impl ArchiveFormat {
    /// The canonical file extension for the format.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Tar => "tar",
            Self::TarGz => "tar.gz",
            Self::Zip => "zip",
        }
    }

    /// Detect the format from a file name's extension.
    ///
    /// Unknown extensions answer `None` — the caller decides
    /// whether that is an error (Wings routes by extension).
    pub fn detect(file_name: &str) -> Option<Self> {
        let lower = file_name.to_ascii_lowercase();
        if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
            Some(Self::TarGz)
        } else if lower.ends_with(".tar") {
            Some(Self::Tar)
        } else if lower.ends_with(".zip") {
            Some(Self::Zip)
        } else {
            None
        }
    }

    /// Whether a file name carries an archive extension (the
    /// nested-archive recursion's detector).
    fn is_archive_name(file_name: &str) -> bool {
        Self::detect(file_name).is_some()
    }
}

/// The §2.7 extraction limits.
///
/// Defaults match the threat model: 5 GiB uncompressed, 100k
/// entries, 3 levels of archive-in-archive recursion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractLimits {
    /// Maximum cumulative uncompressed bytes written by one
    /// extraction (including nested levels).
    pub max_uncompressed_bytes: u64,
    /// Maximum entries unpacked by one extraction (including nested
    /// levels).
    pub max_entries: u64,
    /// Maximum archive-in-archive recursion depth (1 = the top
    /// archive only; the default 3 matches §2.7).
    pub max_nesting_depth: u8,
}

impl Default for ExtractLimits {
    fn default() -> Self {
        Self {
            max_uncompressed_bytes: 5 * 1024 * 1024 * 1024,
            max_entries: 100_000,
            max_nesting_depth: 3,
        }
    }
}

/// What one `extract` call unpacked (bounded-work accounting).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtractStats {
    /// Entries unpacked (files and directories, all levels).
    pub entries: u64,
    /// Uncompressed bytes written (all levels).
    pub uncompressed_bytes: u64,
    /// Nested archives recursed into.
    pub nested_archives: u64,
}

/// A running budget shared across the recursion (§2.7's caps are
/// per top-level extraction, not per archive layer).
#[derive(Debug, Default)]
struct Budget {
    entries: u64,
    bytes: u64,
    nested: u64,
}

impl Budget {
    fn charge_entry(&mut self, limits: &ExtractLimits) -> Result<(), FileError> {
        self.entries += 1;
        if self.entries > limits.max_entries {
            return Err(FileError::Limit(format!(
                "extraction exceeded the {}-entry cap",
                limits.max_entries
            )));
        }
        Ok(())
    }

    /// Charge the declared size up front (§2.7's zip-bomb guard:
    /// the copy is refused before it begins, not after the disk
    /// is full).
    fn charge_bytes(&mut self, len: u64, limits: &ExtractLimits) -> Result<(), FileError> {
        self.bytes = self.bytes.saturating_add(len);
        if self.bytes > limits.max_uncompressed_bytes {
            return Err(FileError::Limit(format!(
                "extraction exceeded the {}-byte uncompressed cap",
                limits.max_uncompressed_bytes
            )));
        }
        Ok(())
    }
}

/// One file or directory collected for archiving.
#[derive(Debug)]
struct Collected {
    /// The relative path inside the archive (forward slashes).
    relative: String,
    /// The absolute source location.
    absolute: PathBuf,
    /// Whether this is a directory entry.
    is_dir: bool,
    /// The file's size (0 for directories).
    size: u64,
}

/// Collect the allow-listed paths (recursively for directories),
/// sorted for deterministic archives.
///
/// Paths are validated through [`SafePath`][crate::SafePath] — an
/// allow-list entry that escapes the root is rejected exactly like
/// an interactive request. Symlinked allow-list entries and
/// in-tree symlinks are followed only when the canonical target
/// stays inside the root (§2.6's symlink rule); the archive stores
/// the target's bytes, which is what Wings's SafePath-wrapped
/// archiver does.
fn collect(root: &Path, allow: &[&str]) -> Result<Vec<Collected>, FileError> {
    let mut out: Vec<Collected> = Vec::new();
    for rel in allow {
        let safe = crate::SafePath::resolve(root, rel)?;
        let abs = safe.canonical().to_path_buf();
        // Missing allow-list entries are skipped (Wings's archiver
        // skips files deleted mid-archive; the request is for what
        // exists).
        let meta = match fs::metadata(&abs) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if meta.is_dir() {
            walk_dir(&abs, safe.relative(), &mut out)?;
        } else {
            out.push(Collected {
                relative: safe.relative().to_owned(),
                absolute: abs,
                is_dir: false,
                size: meta.len(),
            });
        }
    }
    out.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(out)
}

/// Append `dir`'s subtree (and the directory entry itself).
fn walk_dir(dir: &Path, relative: &str, out: &mut Vec<Collected>) -> Result<(), FileError> {
    out.push(Collected {
        relative: relative.to_owned(),
        absolute: dir.to_path_buf(),
        is_dir: true,
        size: 0,
    });
    let mut children: Vec<(String, PathBuf, bool, u64)> = Vec::new();
    let entries = fs::read_dir(dir)
        .map_err(|e| FileError::io(format!("cannot list {}", dir.display()), e))?;
    for entry in entries {
        let entry =
            entry.map_err(|e| FileError::io(format!("cannot read {} entry", dir.display()), e))?;
        let name = entry.file_name();
        let name = name.to_string_lossy().into_owned();
        let child_rel = if relative.is_empty() {
            name.clone()
        } else {
            format!("{relative}/{name}")
        };
        // entry.metadata() follows symlinks — a symlinked child
        // archives its target's shape; escapes were already caught
        // by the SafePath resolution of the allow-list ancestor.
        let meta = entry
            .metadata()
            .map_err(|e| FileError::io(format!("cannot stat {child_rel}"), e))?;
        children.push((child_rel, entry.path(), meta.is_dir(), meta.len()));
    }
    children.sort_by(|a, b| a.0.cmp(&b.0));
    for (child_rel, child_abs, is_dir, size) in children {
        if is_dir {
            walk_dir(&child_abs, &child_rel, out)?;
        } else {
            out.push(Collected {
                relative: child_rel,
                absolute: child_abs,
                is_dir: false,
                size,
            });
        }
    }
    Ok(())
}

/// Compress the allow-listed `paths` under `root` into `dest`
/// (root-relative) as `format`, returning the written byte count.
///
/// `dest` is resolved through [`SafePath`][crate::SafePath] like
/// every other path.
pub fn archive(
    root: &Path,
    dest: &str,
    paths: &[&str],
    format: ArchiveFormat,
) -> Result<u64, FileError> {
    let collected = collect(root, paths)?;
    if collected.is_empty() {
        return Err(FileError::NotFound(dest.to_owned()));
    }
    let dest_safe = crate::SafePath::resolve(root, dest)?;
    let dest_abs = dest_safe.canonical();
    if let Some(parent) = dest_abs.parent() {
        fs::create_dir_all(parent).map_err(|e| FileError::io("cannot create archive parent", e))?;
    }
    match format {
        ArchiveFormat::Tar => {
            let file =
                File::create(dest_abs).map_err(|e| FileError::io("cannot create archive", e))?;
            let mut builder = Builder::new(file);
            write_tar_entries(&mut builder, &collected)?;
            builder
                .finish()
                .map_err(|e| FileError::io("cannot finalize tar", e))?;
        }
        ArchiveFormat::TarGz => {
            let file =
                File::create(dest_abs).map_err(|e| FileError::io("cannot create archive", e))?;
            let gz = GzEncoder::new(file, Compression::default());
            let mut builder = Builder::new(gz);
            write_tar_entries(&mut builder, &collected)?;
            let file = builder
                .into_inner()
                .map_err(|e| FileError::io("cannot finalize tar.gz", e))?
                .finish()
                .map_err(|e| FileError::io("cannot finalize gzip stream", e))?;
            file.sync_all().ok();
        }
        ArchiveFormat::Zip => {
            let file =
                File::create(dest_abs).map_err(|e| FileError::io("cannot create archive", e))?;
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default()
                .large_file(true)
                .compression_method(zip::CompressionMethod::Deflated);
            for entry in &collected {
                if entry.is_dir {
                    zip.add_directory(format!("{}/", entry.relative), options)
                        .map_err(zip_err)?;
                } else {
                    zip.start_file(entry.relative.clone(), options)
                        .map_err(zip_err)?;
                    let mut reader = File::open(&entry.absolute)
                        .map_err(|e| FileError::io("cannot read archive source", e))?;
                    io::copy(&mut reader, &mut zip)
                        .map_err(|e| FileError::io("cannot write zip entry", e))?;
                }
            }
            zip.finish().map_err(zip_err)?;
        }
    }
    let meta =
        fs::metadata(dest_abs).map_err(|e| FileError::io("cannot stat written archive", e))?;
    Ok(meta.len())
}

/// Emit the collected entries through a tar builder (shared by the
/// plain-tar and tar.gz writers).
fn write_tar_entries<W: Write>(
    builder: &mut Builder<W>,
    collected: &[Collected],
) -> Result<(), FileError> {
    for entry in collected {
        let mut header = Header::new_gnu();
        // A stable zero timestamp keeps archives deterministic
        // (identical trees produce byte-identical archives).
        header.set_mtime(0);
        // The size field must be explicitly set for directories too
        // (a zeroed-but-unformatted field fails the reader's octal
        // parse: "numeric field was not a number").
        header.set_size(entry.size);
        if entry.is_dir {
            header.set_entry_type(EntryType::Directory);
            header.set_mode(0o755);
            builder
                .append_data(&mut header, &entry.relative, io::empty())
                .map_err(|e| FileError::io("cannot append tar entry", e))?;
        } else {
            header.set_entry_type(EntryType::Regular);
            header.set_mode(0o644);
            let mut reader = File::open(&entry.absolute)
                .map_err(|e| FileError::io("cannot read archive source", e))?;
            builder
                .append_data(&mut header, &entry.relative, &mut reader)
                .map_err(|e| FileError::io("cannot append tar entry", e))?;
        }
    }
    Ok(())
}

/// Map a zip crate error into the file vocabulary.
fn zip_err(error: zip::result::ZipError) -> FileError {
    FileError::io("zip archive", io::Error::other(error))
}

/// Extract `archive_path` (root-relative) under `dest` (root-relative)
/// with the default [`ExtractLimits`]; see [`extract_with_limits`].
pub fn extract(root: &Path, archive_path: &str, dest: &str) -> Result<ExtractStats, FileError> {
    extract_with_limits(root, archive_path, dest, &ExtractLimits::default())
}

/// Extract with explicit limits; the §2.7 surface (see the module
/// docs for the defenses).
pub fn extract_with_limits(
    root: &Path,
    archive_path: &str,
    dest: &str,
    limits: &ExtractLimits,
) -> Result<ExtractStats, FileError> {
    let archive_safe = crate::SafePath::resolve(root, archive_path)?;
    let dest_safe = crate::SafePath::resolve(root, dest)?;
    let mut format = Archive::open(archive_safe.canonical())?;
    let dest_abs = dest_safe.canonical();
    fs::create_dir_all(dest_abs)
        .map_err(|e| FileError::io("cannot create extraction destination", e))?;
    let mut budget = Budget::default();
    extract_archive(&mut format, dest_abs, limits, &mut budget, 1)?;
    Ok(ExtractStats {
        entries: budget.entries,
        uncompressed_bytes: budget.bytes,
        nested_archives: budget.nested,
    })
}

/// One opened archive, format-detected and ready for iteration.
enum Archive {
    /// A plain tar reader.
    Tar(tar::Archive<File>),
    /// A gzip-decoded tar reader.
    TarGz(tar::Archive<flate2::read::GzDecoder<File>>),
    /// A zip reader.
    Zip(zip::ZipArchive<File>),
}

impl Archive {
    fn open(path: &Path) -> Result<Self, FileError> {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let file = File::open(path).map_err(|e| FileError::io("cannot open archive", e))?;
        match ArchiveFormat::detect(&name) {
            Some(ArchiveFormat::Tar) => Ok(Self::Tar(tar::Archive::new(file))),
            Some(ArchiveFormat::TarGz) => Ok(Self::TarGz(tar::Archive::new(
                flate2::read::GzDecoder::new(file),
            ))),
            Some(ArchiveFormat::Zip) => {
                let zip = zip::ZipArchive::new(file).map_err(zip_err)?;
                Ok(Self::Zip(zip))
            }
            None => Err(FileError::UnsupportedFormat(name)),
        }
    }
}

/// Unpack one archive layer into `dest`, recursing into nested
/// archives while the depth budget allows.
fn extract_archive(
    archive: &mut Archive,
    dest: &Path,
    limits: &ExtractLimits,
    budget: &mut Budget,
    depth: u8,
) -> Result<(), FileError> {
    // Every entry path maps through the lexical validator BEFORE
    // any filesystem write (§2.7: traversal-aware entry names), and
    // no path may repeat within one extraction (overlap rule).
    let mut seen: Vec<PathBuf> = Vec::new();
    match archive {
        Archive::Tar(tar) => extract_tar(tar, dest, limits, budget, depth, &mut seen),
        Archive::TarGz(tar) => extract_tar(tar, dest, limits, budget, depth, &mut seen),
        Archive::Zip(zip) => extract_zip(zip, dest, limits, budget, depth, &mut seen),
    }
}

/// The tar/tar.gz walk (generic over the reader so the two formats
/// share one implementation).
fn extract_tar<R: Read>(
    archive: &mut tar::Archive<R>,
    dest: &Path,
    limits: &ExtractLimits,
    budget: &mut Budget,
    depth: u8,
    seen: &mut Vec<PathBuf>,
) -> Result<(), FileError> {
    for entry in archive.entries().map_err(tar_io_err)? {
        let mut entry = entry.map_err(tar_io_err)?;
        let kind = entry.header().entry_type();
        let name = entry
            .path()
            .map_err(tar_io_err)?
            .to_string_lossy()
            .into_owned();
        // Entry-kind allow-list: regular files and directories only
        // (symlink/hard-link entries are §2.7 rejections).
        if !matches!(kind, EntryType::Regular | EntryType::Directory) {
            return Err(FileError::LinkEntry(name));
        }
        let target = entry_target(&name, dest)?;
        charge_overlap(&target, seen)?;
        budget.charge_entry(limits)?;
        if kind == EntryType::Directory {
            fs::create_dir_all(&target).map_err(|e| FileError::io("cannot create directory", e))?;
        } else {
            let declared = entry.size();
            budget.charge_bytes(declared, limits)?;
            unpack_file(&mut entry, &target)?;
            maybe_recurse(&target, &name, dest, limits, budget, depth)?;
        }
    }
    Ok(())
}

/// The zip walk.
fn extract_zip(
    zip: &mut zip::ZipArchive<File>,
    dest: &Path,
    limits: &ExtractLimits,
    budget: &mut Budget,
    depth: u8,
    seen: &mut Vec<PathBuf>,
) -> Result<(), FileError> {
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(zip_err)?;
        let name = entry.name().to_owned();
        if entry.is_dir() {
            let target = entry_target(&name, dest)?;
            charge_overlap(&target, seen)?;
            budget.charge_entry(limits)?;
            fs::create_dir_all(&target).map_err(|e| FileError::io("cannot create directory", e))?;
            continue;
        }
        // Zip symlink entries carry the file-type bits in the high
        // mode bits; anything that is not a plain file is a link
        // entry and rejected (§2.7).
        if let Some(mode) = entry.unix_mode() {
            if mode & 0o170000 != 0o100000 {
                return Err(FileError::LinkEntry(name));
            }
        }
        let target = entry_target(&name, dest)?;
        charge_overlap(&target, seen)?;
        budget.charge_entry(limits)?;
        let declared = entry.size();
        budget.charge_bytes(declared, limits)?;
        unpack_file(&mut entry, &target)?;
        maybe_recurse(&target, &name, dest, limits, budget, depth)?;
    }
    Ok(())
}

/// Write one unpacked regular file (parent dirs first).
fn unpack_file<R: Read>(entry: &mut R, target: &Path) -> Result<(), FileError> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| FileError::io("cannot create parent", e))?;
    }
    let mut out = fs::File::create(target).map_err(|e| FileError::io("cannot create file", e))?;
    io::copy(entry, &mut out).map_err(|e| FileError::io("cannot unpack entry", e))?;
    out.sync_all().ok();
    Ok(())
}

/// Validate an archive entry name and map it to a concrete target
/// under `dest`.
fn entry_target(name: &str, dest: &Path) -> Result<PathBuf, FileError> {
    if name.contains('\0') || name.contains('\\') {
        return Err(FileError::Traversal(name.to_owned()));
    }
    if name.starts_with('/') {
        return Err(FileError::Absolute(name.to_owned()));
    }
    for component in Path::new(name).components() {
        match component {
            std::path::Component::Normal(_) | std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                return Err(FileError::Traversal(name.to_owned()));
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(FileError::Absolute(name.to_owned()));
            }
        }
    }
    Ok(dest.join(name))
}

/// Reject overlapping entries (§2.7): a path an earlier entry in
/// the same extraction already created.
fn charge_overlap(target: &Path, seen: &mut Vec<PathBuf>) -> Result<(), FileError> {
    if seen.iter().any(|seen_path| seen_path == target) {
        return Err(FileError::Overlap(target.to_string_lossy().into_owned()));
    }
    seen.push(target.to_path_buf());
    Ok(())
}

/// Recurse into a just-written entry that is itself an archive,
/// while the nesting depth allows it; at the cap the bytes stay
/// stored as a plain file (see the module docs).
fn maybe_recurse(
    target: &Path,
    name: &str,
    dest: &Path,
    limits: &ExtractLimits,
    budget: &mut Budget,
    depth: u8,
) -> Result<(), FileError> {
    let file_name = Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !ArchiveFormat::is_archive_name(&file_name) || depth >= limits.max_nesting_depth {
        return Ok(());
    }
    budget.nested += 1;
    let mut nested = Archive::open(target)?;
    // The nested archive unpacks beside itself (its own directory),
    // matching Wings's decompress-where-it-stands behavior.
    let nested_dest = target.parent().unwrap_or(dest).to_path_buf();
    extract_archive(&mut nested, &nested_dest, limits, budget, depth + 1)
}

/// Map a tar crate IO-flavored error.
fn tar_io_err(error: io::Error) -> FileError {
    FileError::io("tar archive", error)
}
