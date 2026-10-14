//! Path safety for the file API (M17).
//!
//! Every file-API operation funnels through [`SafePath`]: a
//! user-supplied *relative* path is validated lexically (absolute
//! paths, `..` components, and NUL bytes are rejected before any
//! filesystem call) and then resolved against the instance's data
//! directory, with the resolved location re-verified to still be
//! inside it. This is the two-step defense
//! `docs/security/THREAT_MODEL.md` §2.6 requires:
//!
//! 1. lexical rejection of traversal (`../` sequences, absolute
//!    paths) — cheap, and it also rejects malformed archive entry
//!    names during extraction (§2.7);
//! 2. symlink-aware resolution: `fs::canonicalize` follows every
//!    symlink on the way and the canonical result must carry the
//!    data directory's canonical path as a prefix. A symlink whose
//!    target escapes the data directory therefore fails resolution
//!    (`FileError::EscapesRoot`), never the traversal check — which
//!    is exactly the "symlinks are followed only if the target is
//!    within the data directory" rule.
//!
//! The `openat2(RESOLVE_BENEATH)` hardening §2.6 documents for Linux
//! 5.6+ is a defense-in-depth *addition* to this scheme and remains
//! deferred (the std library exposes no `openat` family yet); the
//! canonicalize-and-recheck path here is the documented fallback and
//! is what Wings's `ValidatePath`-then-`SafePath` sequence does.
//!
//! # TOCTOU note
//!
//! Lexical validation and canonical resolution happen at operation
//! time, and the actual open happens immediately after on the
//! *canonical* path — the window between re-check and open is the
//! std-library one shared with Wings's own implementation; §2.6's
//! test requirements cover the practical attacks (see
//! `tests/files.rs`).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::{Component, Path, PathBuf};

use crate::FileError;

/// A validated, root-confined path into an instance's data
/// directory.
///
/// Construct through [`SafePath::resolve`]; the struct carries both
/// the canonical location and the lexical (user-shaped) relative
/// form so listings can echo the requested spelling back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafePath {
    /// The canonical absolute location (symlinks resolved, verified
    /// inside the root's canonical form).
    canonical: PathBuf,
    /// The validated relative form (forward-slash separated).
    relative: String,
}

impl SafePath {
    /// Validate and resolve `relative` against `root`.
    ///
    /// `root` itself is canonicalized once (callers cache via the
    /// [`Filesystem`][crate::Filesystem] handle; the cost is one
    /// syscall per operation here). See the module docs for the
    /// two-step defense.
    pub fn resolve(root: &Path, relative: &str) -> Result<Self, FileError> {
        let relative = relative.trim();
        if relative.is_empty() {
            return Err(FileError::EmptyPath);
        }
        if relative.contains('\0') {
            return Err(FileError::Traversal(relative.to_owned()));
        }
        // Windows-shaped absolute paths ("C:\...", "\\server\...") and
        // backslash separators are rejected rather than interpreted:
        // the API's path grammar is forward-slash-relative only.
        if relative.contains('\\') {
            return Err(FileError::Traversal(relative.to_owned()));
        }

        // ── Step 1: lexical validation on components ─────────────
        let mut components: Vec<&str> = Vec::new();
        for component in Path::new(relative).components() {
            match component {
                Component::Normal(part) => {
                    let part = part.to_str().ok_or_else(|| {
                        FileError::Traversal(format!("non-UTF-8 component in {relative:?}"))
                    })?;
                    components.push(part);
                }
                Component::CurDir => {} // "." components are noise
                Component::ParentDir => {
                    return Err(FileError::Traversal(relative.to_owned()));
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(FileError::Absolute(relative.to_owned()));
                }
            }
        }
        if components.is_empty() {
            return Err(FileError::EmptyPath);
        }
        let joined = components.join("/");

        // ── Step 2: symlink-aware resolution + containment ───────
        let canonical_root = root.canonicalize().map_err(|e| {
            FileError::io(format!("cannot resolve data root {}", root.display()), e)
        })?;
        let candidate = canonical_root.join(&joined);
        // The candidate's *existing* prefix must canonicalize inside
        // the root. A missing tail (the common write case) resolves
        // through its deepest existing ancestor; the non-existing
        // remainder is appended lexically and stays inside by
        // construction (no ".." components, no symlinks to follow).
        let (existing, tail) = split_existing(&candidate);
        let canonical_existing = existing
            .canonicalize()
            .map_err(|e| FileError::io(format!("cannot resolve {}", existing.display()), e))?;
        // Joining an empty remainder is skipped — `join("")` leaves
        // a trailing separator, and a trailing separator turns later
        // stat/create/rename calls into `ENOTDIR`.
        let canonical = if tail.as_os_str().is_empty() {
            canonical_existing
        } else {
            canonical_existing.join(tail)
        };
        if !canonical.starts_with(&canonical_root) {
            return Err(FileError::EscapesRoot(relative.to_owned()));
        }
        // A canonical equality with the root itself is only legal
        // when the caller asked for the root's own listing; the
        // Filesystem layer decides that. SafePath carries it.
        Ok(Self {
            canonical,
            relative: joined,
        })
    }

    /// The canonical absolute location.
    pub fn canonical(&self) -> &Path {
        &self.canonical
    }

    /// The validated relative form (forward-slash separated, no
    /// `..`/`.` components).
    pub fn relative(&self) -> &str {
        &self.relative
    }

    /// The number of path components (nesting depth; used by the
    /// extraction limits).
    pub fn depth(&self) -> usize {
        self.relative.matches('/').count() + 1
    }
}

/// Split `path` into its deepest existing ancestor and the
/// non-existing remainder (both lexical, no symlink resolution).
///
/// The remainder is rebuilt by pushing components onto an empty
/// path — `Path::join` with an empty seed would leave a trailing
/// separator, and a trailing separator turns the later
/// `rename`/`create` into `ENOTDIR`.
fn split_existing(path: &Path) -> (PathBuf, PathBuf) {
    let mut existing = path.to_path_buf();
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        match existing.parent() {
            Some(parent) if parent != existing => {
                if let Some(name) = existing.file_name() {
                    parts.push(name.to_os_string());
                }
                existing = parent.to_path_buf();
            }
            // A root-anchored path with no parent left: treat what
            // exists (the root) as the existing prefix.
            _ => break,
        }
    }
    let mut tail = PathBuf::new();
    for part in parts.iter().rev() {
        tail.push(part);
    }
    (existing, tail)
}

#[cfg(test)]
mod tests {
    //! Pure-lexical validation coverage; the symlink/escape attacks
    //! need a real tree and live in `tests/files.rs`.

    use super::*;

    fn tmp_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!("zim-files-path-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create temp root");
        root
    }

    #[test]
    fn plain_relative_paths_resolve_inside_the_root() {
        let root = tmp_root();
        let safe = SafePath::resolve(&root, "server.properties").unwrap();
        assert_eq!(safe.relative(), "server.properties");
        assert!(safe.canonical().starts_with(root.canonicalize().unwrap()));
        assert_eq!(safe.depth(), 1);

        let safe = SafePath::resolve(&root, "worlds/world/level.dat").unwrap();
        assert_eq!(safe.relative(), "worlds/world/level.dat");
        assert_eq!(safe.depth(), 3);
    }

    #[test]
    fn dot_components_are_noise() {
        let root = tmp_root();
        let safe = SafePath::resolve(&root, "./plugins/./essentials.jar").unwrap();
        assert_eq!(safe.relative(), "plugins/essentials.jar");
    }

    #[test]
    fn traversal_is_rejected_lexically() {
        let root = tmp_root();
        for bad in [
            "..",
            "../outside.txt",
            "worlds/../../etc/passwd",
            "a/b/../..",
        ] {
            assert!(
                matches!(SafePath::resolve(&root, bad), Err(FileError::Traversal(_))),
                "{bad:?} must be traversal-rejected"
            );
        }
    }

    #[test]
    fn absolute_and_windows_shapes_are_rejected() {
        let root = tmp_root();
        for bad in [
            "/etc/passwd",
            "C:\\Windows\\system32",
            "\\\\server\\share",
            "a\\b",
        ] {
            let err = SafePath::resolve(&root, bad).unwrap_err();
            assert!(
                matches!(err, FileError::Absolute(_) | FileError::Traversal(_)),
                "{bad:?} must be shape-rejected, got {err:?}"
            );
        }
    }

    #[test]
    fn empty_and_nul_paths_are_rejected() {
        let root = tmp_root();
        assert!(matches!(
            SafePath::resolve(&root, ""),
            Err(FileError::EmptyPath)
        ));
        assert!(matches!(
            SafePath::resolve(&root, "   "),
            Err(FileError::EmptyPath)
        ));
        assert!(matches!(
            SafePath::resolve(&root, "a\0b"),
            Err(FileError::Traversal(_))
        ));
    }

    #[test]
    fn dot_only_paths_are_empty_after_normalization() {
        let root = tmp_root();
        assert!(matches!(
            SafePath::resolve(&root, "."),
            Err(FileError::EmptyPath)
        ));
    }
}
