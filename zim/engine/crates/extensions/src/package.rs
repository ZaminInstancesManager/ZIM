//! The loaded addon package: an in-memory file tree plus its
//! validated manifest, and the `application/zip` upload format
//! (`POST /api/v1/extensions`, §4.11).
//!
//! Addon packages are ZIP archives with a `zim.addon.json` manifest
//! at the root — the same discipline as frontend-package uploads, so
//! one authoring toolchain covers both. Limits mirror the frontend
//! loader's.

use std::collections::BTreeMap;
use std::io::Read;

use crate::manifest::{AddonManifest, ManifestError, MANIFEST_FILE};

/// Every way loading an addon package can fail.
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
    /// The uploaded archive is not a usable ZIP.
    #[error("the upload is not a valid ZIP archive: {0}")]
    BadArchive(String),
    /// The archive broke package limits.
    #[error("the upload exceeds package limits: {0}")]
    Limit(String),
}

/// Hard limits on uploaded addon packages (mirrors the frontend
/// loader's: 32 MiB / 1000 files is a generous ceiling for any real
/// addon UI).
pub const MAX_PACKAGE_BYTES: u64 = 32 * 1024 * 1024;
/// Maximum number of files in an uploaded package.
pub const MAX_PACKAGE_FILES: usize = 1000;
/// Maximum uncompressed size of a single file entry.
const MAX_ENTRY_BYTES: u64 = 32 * 1024 * 1024;

/// A loaded addon package.
#[derive(Debug, Clone)]
pub struct AddonPackage {
    /// The validated manifest.
    pub manifest: AddonManifest,
    /// Package-root-relative path → bytes (manifest included).
    files: BTreeMap<String, Vec<u8>>,
}

impl AddonPackage {
    /// Load a package from ZIP bytes (`POST /api/v1/extensions`'s
    /// body).
    pub fn from_zip(bytes: &[u8]) -> Result<Self, PackageError> {
        if bytes.len() as u64 > MAX_PACKAGE_BYTES {
            return Err(PackageError::Limit(format!(
                "{} bytes exceeds the {}-byte ceiling",
                bytes.len(),
                MAX_PACKAGE_BYTES
            )));
        }
        let cursor = std::io::Cursor::new(bytes);
        let mut archive =
            zip::ZipArchive::new(cursor).map_err(|e| PackageError::BadArchive(e.to_string()))?;
        if archive.len() > MAX_PACKAGE_FILES {
            return Err(PackageError::Limit(format!(
                "{} entries exceeds the {}-entry ceiling",
                archive.len(),
                MAX_PACKAGE_FILES
            )));
        }
        let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for index in 0..archive.len() {
            let mut entry = archive
                .by_index(index)
                .map_err(|e| PackageError::BadArchive(e.to_string()))?;
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().to_owned();
            // Confinement: the archive may not reference anything
            // outside its own root (no `..`, no absolute paths, no
            // backslashes, no drive letters).
            if name.starts_with('/')
                || name.contains('\\')
                || name.split('/').any(|s| s.is_empty() || s == "." || s == "..")
                || (name.len() > 2 && name.as_bytes()[1] == b':')
            {
                return Err(PackageError::BadArchive(format!(
                    "entry {name:?} escapes the package root"
                )));
            }
            if entry.size() > MAX_ENTRY_BYTES {
                return Err(PackageError::Limit(format!(
                    "entry {name:?} is {} bytes; the per-file ceiling is {}",
                    entry.size(),
                    MAX_ENTRY_BYTES
                )));
            }
            let mut data = Vec::with_capacity(entry.size() as usize);
            entry
                .by_ref()
                .take(MAX_ENTRY_BYTES)
                .read_to_end(&mut data)
                .map_err(|e| PackageError::BadArchive(format!("reading {name:?}: {e}")))?;
            files.insert(name, data);
        }

        let manifest_bytes = files.get(MANIFEST_FILE).ok_or_else(|| {
            PackageError::BadArchive(format!("the archive has no {MANIFEST_FILE} at its root"))
        })?;
        let manifest = AddonManifest::from_bytes(manifest_bytes)?;

        // Every declared entrypoint must exist in the archive.
        for (surface, path) in &manifest.entrypoints {
            if !files.contains_key(path) {
                return Err(PackageError::MissingFile {
                    field: "entrypoints",
                    path: format!("{surface} → {path}"),
                });
            }
        }
        Ok(Self { manifest, files })
    }

    /// The file table (path → bytes), manifest included.
    pub fn files(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.files
    }

    /// One file's bytes, by package-relative path.
    pub fn file(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(Vec::as_slice)
    }

    /// The manifest bytes as stored.
    pub fn manifest_bytes(&self) -> &[u8] {
        self.files
            .get(MANIFEST_FILE)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::InstanceScope;
    use std::collections::BTreeMap;

    fn write_zip(files: &[(&str, &str)]) -> Vec<u8> {
        let mut buffer = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
            for (name, contents) in files {
                writer
                    .start_file(
                        *name,
                        zip::write::SimpleFileOptions::default()
                            .compression_method(zip::CompressionMethod::Deflated),
                    )
                    .expect("start entry");
                    std::io::Write::write_all(&mut writer, contents.as_bytes())
                        .expect("write entry");
            }
            writer.finish().expect("finish archive");
        }
        buffer
    }

    fn manifest_json() -> String {
        serde_json::json!({
            "manifest_version": 1,
            "id": "org.example.zim-addon.test",
            "version": "0.1.0",
            "name": "Test Addon",
            "description": "A test addon.",
            "author": "Tests",
            "license": "MIT",
            "api_version": "v1",
            "engines": { "zim": ">=0.0.1" },
            "permissions": ["websocket.connect"],
            "instance_scope": "optional",
            "page_surfaces": ["zim://servers/{id}/testpane"],
            "entrypoints": { "testpane": "pages/testpane.html" },
            "event_subscriptions": [],
            "dependencies": []
        })
        .to_string()
    }

    #[test]
    fn zip_round_trip() {
        let archive = write_zip(&[
            (MANIFEST_FILE, &manifest_json()),
            ("pages/testpane.html", "<p>hello</p>"),
            ("assets/app.js", "console.log(1);"),
        ]);
        let package = AddonPackage::from_zip(&archive).expect("the round-trip archive loads");
        assert_eq!(package.manifest.id, "org.example.zim-addon.test");
        assert_eq!(package.files().len(), 3);
        assert_eq!(
            std::str::from_utf8(package.file("pages/testpane.html").unwrap()).unwrap(),
            "<p>hello</p>"
        );
    }

    #[test]
    fn zip_rejects_escaped_paths_and_missing_manifests() {
        let archive = write_zip(&[("../escape.txt", "no")]);
        assert!(AddonPackage::from_zip(&archive).is_err());

        let archive = write_zip(&[("pages/only.html", "<p>no manifest</p>")]);
        assert!(matches!(
            AddonPackage::from_zip(&archive),
            Err(PackageError::BadArchive(_))
        ));
    }

    #[test]
    fn zip_rejects_missing_entrypoint_files() {
        let archive = write_zip(&[(MANIFEST_FILE, &manifest_json())]);
        assert!(matches!(
            AddonPackage::from_zip(&archive),
            Err(PackageError::MissingFile { field: "entrypoints", .. })
        ));
    }

    #[test]
    fn zip_rejects_invalid_manifests() {
        let archive = write_zip(&[(MANIFEST_FILE, "{\"id\": 1}")]);
        assert!(matches!(
            AddonPackage::from_zip(&archive),
            Err(PackageError::Manifest(_))
        ));
    }

    /// The manifest defaults: a minimal manifest with only the
    /// identity fields validates (the contribution lists default
    /// empty) — an addon may be pure event-subscriber with no UI.
    #[test]
    fn minimal_manifest_with_no_surfaces_validates() {
        let minimal = serde_json::json!({
            "manifest_version": 1,
            "id": "org.example.minimal",
            "version": "0.0.1",
            "name": "Minimal",
            "api_version": "v1",
            "engines": { "zim": ">=0.0.1" },
            "permissions": [],
            "instance_scope": "none",
            "event_subscriptions": ["job.completed"]
        })
        .to_string();
        let archive = write_zip(&[(MANIFEST_FILE, &minimal)]);
        let package = AddonPackage::from_zip(&archive).unwrap();
        assert_eq!(package.manifest.instance_scope, InstanceScope::None);
        assert!(package.manifest.page_surfaces.is_empty());
        assert!(package.manifest.entrypoints.is_empty());
        // BTreeMap default: engines present, entrypoints default.
        let _: BTreeMap<String, String> = package.manifest.engines.clone();
    }
}
