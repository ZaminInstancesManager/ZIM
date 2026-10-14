//! The loaded frontend package: an in-memory file tree plus its
//! validated manifest, and the `zim://` surface routing the engine
//! serves it through.
//!
//! Packages come from three places: the bundled default (embedded at
//! build time via `build.rs`), the engine's data directory (an
//! uploaded, extracted package), or an upload archive (§10.14).

use std::collections::BTreeMap;
use std::path::Path;

use crate::manifest::{
    safe_package_path, FrontendManifest, ManifestError, SurfacePattern, MANIFEST_FILE,
};

/// Every way loading a package can fail.
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
pub enum PackageError {
    /// The manifest failed validation.
    #[error("manifest: {0}")]
    Manifest(#[from] ManifestError),
    /// A declared file is missing from the package.
    #[error("the package is missing {path:?} (declared as {field})")]
    MissingFile {
        /// Which declaration pointed at the file.
        field: &'static str,
        /// The missing package-relative path.
        path: String,
    },
    /// A file read failed.
    #[error("cannot read {path:?}: {message}")]
    Io {
        /// The path that failed.
        path: String,
        /// The underlying failure.
        message: String,
    },
    /// The uploaded archive is not a usable ZIP.
    #[error("the upload is not a valid ZIP archive: {0}")]
    BadArchive(String),
    /// The archive broke package limits.
    #[error("the upload exceeds package limits: {0}")]
    Limit(String),
}

/// Hard limits on uploaded packages (§13-adjacent hygiene: the
/// default frontend is ~tens of KB; 32 MiB / 1000 files is a
/// generous ceiling for any real UI package).
pub const MAX_PACKAGE_BYTES: u64 = 32 * 1024 * 1024;
/// Maximum number of files in an uploaded package.
pub const MAX_PACKAGE_FILES: usize = 1000;

/// A loaded frontend package.
#[derive(Debug, Clone)]
pub struct FrontendPackage {
    /// The validated manifest.
    pub manifest: FrontendManifest,
    /// Package-root-relative path → bytes (manifest included).
    files: BTreeMap<String, Vec<u8>>,
}

impl FrontendPackage {
    /// Assemble a package from raw (path, bytes) pairs plus its
    /// manifest bytes.
    pub fn from_parts(manifest: FrontendManifest, files: Vec<(String, Vec<u8>)>) -> Self {
        let mut map = BTreeMap::new();
        map.insert(MANIFEST_FILE.to_owned(), manifest_bytes(&manifest));
        for (path, bytes) in files {
            if path == MANIFEST_FILE {
                continue; // The canonical manifest bytes win.
            }
            map.insert(path, bytes);
        }
        Self {
            manifest,
            files: map,
        }
    }

    /// The embedded default frontend package.
    pub fn default_package() -> Self {
        let mut map = BTreeMap::new();
        for (path, bytes) in default_package_files() {
            map.insert((*path).to_owned(), bytes.to_vec());
        }
        let manifest = FrontendManifest::from_bytes(
            map.get(MANIFEST_FILE)
                .expect("the default package embeds its manifest"),
        )
        .expect("the bundled default frontend manifest is valid");
        Self {
            manifest,
            files: map,
        }
    }

    /// Load a package from an extracted directory in the data dir.
    pub fn from_dir(root: &Path) -> Result<Self, PackageError> {
        let manifest_path = root.join(MANIFEST_FILE);
        let manifest_bytes = std::fs::read(&manifest_path).map_err(|e| PackageError::Io {
            path: MANIFEST_FILE.to_owned(),
            message: e.to_string(),
        })?;
        let manifest = FrontendManifest::from_bytes(&manifest_bytes)?;
        let mut files = vec![(MANIFEST_FILE.to_owned(), manifest_bytes)];
        collect_dir(root, root, &mut files)?;
        Ok(Self::from_parts(manifest, files))
    }

    /// Load a package from uploaded ZIP bytes (`POST /frontend/upload`).
    pub fn from_zip(bytes: &[u8]) -> Result<Self, PackageError> {
        let cursor = std::io::Cursor::new(bytes);
        let mut archive =
            zip::ZipArchive::new(cursor).map_err(|e| PackageError::BadArchive(e.to_string()))?;
        if archive.len() > MAX_PACKAGE_FILES {
            return Err(PackageError::Limit(format!(
                "{} entries (limit {MAX_PACKAGE_FILES})",
                archive.len()
            )));
        }
        let mut total: u64 = 0;
        let mut manifest_bytes: Option<Vec<u8>> = None;
        let mut files = Vec::new();
        for index in 0..archive.len() {
            let mut entry = archive
                .by_index(index)
                .map_err(|e| PackageError::BadArchive(e.to_string()))?;
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().to_owned();
            if !safe_package_path(&name) {
                return Err(PackageError::BadArchive(format!(
                    "entry {name:?} is not a safe package-relative path"
                )));
            }
            total += entry.size();
            if total > MAX_PACKAGE_BYTES {
                return Err(PackageError::Limit(format!(
                    "decompressed size exceeds {} bytes",
                    MAX_PACKAGE_BYTES
                )));
            }
            let mut data = Vec::with_capacity(entry.size() as usize);
            std::io::Read::read_to_end(&mut entry, &mut data)
                .map_err(|e| PackageError::BadArchive(format!("entry {name:?}: {e}")))?;
            if name == MANIFEST_FILE {
                manifest_bytes = Some(data);
            } else {
                files.push((name, data));
            }
        }
        let manifest_bytes = manifest_bytes.ok_or_else(|| {
            PackageError::BadArchive(format!("the archive has no {MANIFEST_FILE} at its root"))
        })?;
        let manifest = FrontendManifest::from_bytes(&manifest_bytes)?;
        Ok(Self::from_parts(manifest, files))
    }

    /// One file's bytes, by package-relative path.
    pub fn file(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(Vec::as_slice)
    }

    /// Every file path (sorted; includes the manifest).
    pub fn paths(&self) -> Vec<String> {
        self.files.keys().cloned().collect()
    }

    /// The package's total byte size.
    pub fn byte_len(&self) -> u64 {
        self.files.values().map(Vec::len).map(|n| n as u64).sum()
    }

    /// The smoke-test page the manifest configures (or the
    /// conventional fallbacks, when present).
    pub fn smoke_page(&self) -> Option<String> {
        if let Some(configured) = &self.manifest.smoke_test {
            return Some(configured.clone());
        }
        for candidate in ["smoke.html", "pages/smoke.html"] {
            if self.files.contains_key(candidate) {
                return Some(candidate.to_owned());
            }
        }
        None
    }

    /// Route one concrete HTTP path (what follows `/zim/`) through
    /// the manifest's declared surfaces.
    ///
    /// Returns the entrypoint file to serve and the extracted
    /// parameters on match. `""` (the package root) routes to the
    /// `newtab` entrypoint when declared — the NTP-replacement rule
    /// (BROWSER_INTEGRATION.md §8).
    pub fn resolve_surface(&self, http_path: &str) -> Option<(String, Vec<(String, String)>)> {
        if http_path.trim_matches('/').is_empty() {
            return self
                .manifest
                .entrypoints
                .get("newtab")
                .map(|file| (file.clone(), Vec::new()));
        }
        for surface in &self.manifest.page_surfaces {
            if let Ok(pattern) = SurfacePattern::parse(surface) {
                if let Some(params) = pattern.matches_path(http_path) {
                    let key = pattern.entrypoint_key();
                    if let Some(file) = self.manifest.entrypoints.get(&key) {
                        return Some((file.clone(), params));
                    }
                }
            }
        }
        None
    }

    /// Write the package out as a directory tree (upload persistence).
    pub fn write_dir(&self, root: &Path) -> Result<(), PackageError> {
        std::fs::create_dir_all(root).map_err(|e| PackageError::Io {
            path: root.display().to_string(),
            message: e.to_string(),
        })?;
        for (path, bytes) in &self.files {
            let target = root.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| PackageError::Io {
                    path: parent.display().to_string(),
                    message: e.to_string(),
                })?;
            }
            std::fs::write(&target, bytes).map_err(|e| PackageError::Io {
                path: path.clone(),
                message: e.to_string(),
            })?;
        }
        Ok(())
    }
}

/// The manifest's own canonical bytes (for `from_parts`).
fn manifest_bytes(manifest: &FrontendManifest) -> Vec<u8> {
    serde_json::to_vec_pretty(manifest).unwrap_or_default()
}

/// The build-generated embedded file table (see `build.rs`).
mod embedded {
    include!(concat!(env!("OUT_DIR"), "/default_package.rs"));
}

/// The embedded default frontend's (path, bytes) pairs.
fn default_package_files() -> &'static [(&'static str, &'static [u8])] {
    embedded::FILES
}

/// Recursively collect a directory's files with root-relative
/// forward-slash paths.
fn collect_dir(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), PackageError> {
    let entries = std::fs::read_dir(dir).map_err(|e| PackageError::Io {
        path: dir.display().to_string(),
        message: e.to_string(),
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_dir(root, &path, out)?;
        } else {
            let relative = path
                .strip_prefix(root)
                .expect("collected path is under root")
                .to_string_lossy()
                .replace('\\', "/");
            if relative == MANIFEST_FILE {
                continue; // Read once, up front.
            }
            let bytes = std::fs::read(&path).map_err(|e| PackageError::Io {
                path: relative.clone(),
                message: e.to_string(),
            })?;
            out.push((relative, bytes));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_package_loads_and_is_valid() {
        let package = FrontendPackage::default_package();
        assert_eq!(package.manifest.id, "org.zaminmc.frontend.default");
        assert!(package.manifest.validate().is_ok());
        // Every declared entrypoint and asset is embedded.
        for path in package.manifest.entrypoints.values() {
            assert!(package.file(path).is_some(), "missing {path}");
        }
        for path in &package.manifest.assets {
            assert!(package.file(path).is_some(), "missing {path}");
        }
        let smoke = package
            .smoke_page()
            .expect("the default declares a smoke page");
        assert!(package.file(&smoke).is_some());
        assert!(package.byte_len() > 0);
    }

    #[test]
    fn the_default_package_routes_its_surfaces() {
        let package = FrontendPackage::default_package();
        let (file, params) = package.resolve_surface("newtab").expect("newtab routes");
        assert_eq!(file, "pages/newtab.html");
        assert!(params.is_empty());

        let (file, _params) = package.resolve_surface("servers").expect("servers routes");
        assert_eq!(file, "pages/servers.html");

        let (file, params) = package
            .resolve_surface("servers/0a1b2c3d-0000-0000-0000-000000000000/console")
            .expect("console routes");
        assert_eq!(file, "pages/console.html");
        assert_eq!(
            params,
            vec![(
                "id".to_owned(),
                "0a1b2c3d-0000-0000-0000-000000000000".to_owned()
            )]
        );

        // The package root → newtab (NTP replacement).
        let (file, _) = package.resolve_surface("").expect("root routes");
        assert_eq!(file, "pages/newtab.html");

        // Undeclared surface → None (the 404 path).
        assert!(package.resolve_surface("settings").is_none());
        assert!(package.resolve_surface("servers/x/files").is_none());
    }

    #[test]
    fn zip_round_trip() {
        // Build the default package into a ZIP in memory, then load
        // it back through the upload path.
        let package = FrontendPackage::default_package();
        let mut buffer = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
            for (path, bytes) in &package.files {
                writer
                    .start_file(
                        path.as_str(),
                        zip::write::SimpleFileOptions::default()
                            .compression_method(zip::CompressionMethod::Deflated),
                    )
                    .unwrap();
                std::io::Write::write_all(&mut writer, bytes).unwrap();
            }
            writer.finish().unwrap();
        }
        let loaded = FrontendPackage::from_zip(&buffer).expect("the round-trip archive loads");
        assert_eq!(loaded.manifest.id, package.manifest.id);
        for path in package.paths() {
            if path == MANIFEST_FILE {
                // from_parts canonicalizes the manifest through the
                // typed struct (e.g. dropping `$schema`); compare the
                // semantics instead of the bytes.
                continue;
            }
            assert_eq!(
                loaded.file(&path),
                package.file(&path).map(<[u8]>::to_vec).as_deref(),
                "{path}"
            );
        }
        assert_eq!(loaded.manifest.entrypoints, package.manifest.entrypoints);
        assert_eq!(
            loaded.manifest.page_surfaces,
            package.manifest.page_surfaces
        );
        assert_eq!(loaded.manifest.assets, package.manifest.assets);
    }

    #[test]
    fn zip_rejects_escaped_paths_and_missing_manifests() {
        let mut buffer = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
            writer
                .start_file("../escape.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut writer, b"escaped").unwrap();
            writer.finish().unwrap();
        }
        assert!(matches!(
            FrontendPackage::from_zip(&buffer),
            Err(PackageError::BadArchive(_))
        ));

        let mut buffer = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
            writer
                .start_file("assets/x.css", zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut writer, b"body{}").unwrap();
            writer.finish().unwrap();
        }
        assert!(matches!(
            FrontendPackage::from_zip(&buffer),
            Err(PackageError::BadArchive(_))
        ));
    }
}
