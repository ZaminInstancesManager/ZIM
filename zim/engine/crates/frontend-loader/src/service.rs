//! The frontend service: the engine's frontend loader.
//!
//! Owns the active package (the embedded default, or the uploaded
//! custom package recorded in the data directory), the activation
//! gate (§3.5), the recovery path (§3.6 and architecture invariant
//! 16), and the `/zim/` request routing surface the API layer
//! serves from.
//!
//! # Layout in the data directory
//!
//! ```text
//! {data_dir}/frontend/
//!   active.json          # {"id": …, "version": …} ("default" id ⇒ bundled)
//!   trusted-keys.json    # [{"id": …, "public_key": "<base32…base64>"}]
//!   packages/{id}/{version}/   # extracted uploads
//! ```
//!
//! # Concurrency
//!
//! The active package lives behind an `RwLock`-swapped `Arc`; page
//! serving never blocks on activation, and activation swaps the
//! whole package atomically.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::manifest::{FrontendManifest, MANIFEST_FILE};
use crate::package::{FrontendPackage, PackageError};
use crate::signature::{SignatureCheck, TrustStore};
use crate::smoke::{self, SmokeReport};

/// Everything the frontend subsystem can fail with; the API layer
/// maps these onto the §6.1 vocabulary.
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
pub enum FrontendError {
    /// The package was not found (no such id/version uploaded).
    #[error("no frontend package {0}")]
    NotFound(String),
    /// The same id+version is already uploaded.
    #[error("frontend package {0} is already uploaded")]
    AlreadyExists(String),
    /// The upload archive is unusable.
    #[error("{0}")]
    BadPackage(#[from] PackageError),
    /// The activation smoke gate failed.
    #[error("the package failed its activation smoke test")]
    SmokeTestFailed(Box<SmokeReport>),
    /// The package is unsigned/by an unknown author and no explicit
    /// admin approval was given.
    #[error("activation requires admin approval: the package signature is {0}")]
    ApprovalRequired(&'static str),
    /// A data-directory failure.
    #[error("frontend storage error: {0}")]
    Storage(String),
}

/// The `{data_dir}/frontend/active.json` record.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ActiveRecord {
    id: String,
    version: Option<String>,
}

/// The operator's `trusted-keys.json` entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TrustedKeyFile {
    keys: Vec<TrustedKeyEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TrustedKeyEntry {
    #[serde(default)]
    id: String,
    public_key: String,
}

/// What one `/zim/…` request resolves to.
#[derive(Debug, Clone)]
pub enum Resolution {
    /// Serve this file (package-relative path) with this MIME type.
    File {
        /// Package-relative path.
        path: String,
        /// Content type.
        mime: &'static str,
        /// Whether the response may be cached (assets: yes, pages: no).
        cacheable: bool,
    },
    /// No declared surface or file matched: serve the engine's
    /// built-in 404 page.
    NotFound,
}

/// The recovery notice surfaced on `zim://newtab` after a revert
/// (§3.6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryNotice {
    /// Why the engine reverted (the load or gate failure).
    pub reason: String,
    /// When the engine reverted.
    pub at: chrono::DateTime<chrono::Utc>,
    /// The package that was reverted away from.
    pub from: String,
}

/// A package's identity summary (upload/activation responses).
#[derive(Debug, Clone, Serialize)]
pub struct PackageSummary {
    /// The manifest's `object` discriminator.
    pub object: &'static str,
    /// Package id.
    pub id: String,
    /// Package version.
    pub version: String,
    /// Human name.
    pub name: String,
    /// The signature classification.
    pub signature: &'static str,
    /// Total byte size.
    pub bytes: u64,
}

impl PackageSummary {
    fn of(package: &FrontendPackage, signature: &SignatureCheck) -> Self {
        Self {
            object: "frontend_package",
            id: package.manifest.id.clone(),
            version: package.manifest.version.clone(),
            name: package.manifest.name.clone(),
            signature: signature.as_str(),
            bytes: package.byte_len(),
        }
    }
}

/// The active-frontend info (`GET /frontend/active`).
#[derive(Debug, Clone, Serialize)]
pub struct ActiveInfo {
    /// The manifest's `object` discriminator.
    pub object: &'static str,
    /// Active package id.
    pub id: String,
    /// Active package version.
    pub version: String,
    /// Human name.
    pub name: String,
    /// Whether the active package is the bundled default.
    pub is_default: bool,
    /// Where the bytes come from.
    pub origin: &'static str,
    /// The declared `zim://` surface inventory.
    pub page_surfaces: Vec<String>,
    /// The signature classification.
    pub signature: &'static str,
    /// The pending recovery notice, if the engine reverted this boot.
    pub recovery_notice: Option<RecoveryNotice>,
}

/// The engine's frontend loader.
pub struct FrontendService {
    data_dir: PathBuf,
    engine_version: semver::Version,
    api_version: String,
    trust: TrustStore,
    active: RwLock<Arc<ActiveFrontend>>,
}

/// The currently serving package plus its origin bookkeeping.
struct ActiveFrontend {
    package: FrontendPackage,
    is_default: bool,
    signature: SignatureCheck,
    recovery: Option<RecoveryNotice>,
}

impl std::fmt::Debug for FrontendService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrontendService")
            .field("data_dir", &self.data_dir)
            .field("engine_version", &self.engine_version)
            .finish_non_exhaustive()
    }
}

/// The default package's manifest id (the reset target).
pub const DEFAULT_ID: &str = "org.zaminmc.frontend.default";

impl FrontendService {
    /// Build the service: load the recorded active package — or the
    /// bundled default — running the full gate (and the §3.6
    /// recovery path) before serving anything.
    pub fn new(
        data_dir: impl AsRef<Path>,
        engine_version: semver::Version,
        api_version: impl Into<String>,
    ) -> Self {
        let data_dir = data_dir.as_ref().join("frontend");
        let trust = load_trust_store(&data_dir);
        let service = Self {
            data_dir,
            engine_version,
            api_version: api_version.into(),
            trust,
            active: RwLock::new(Arc::new(ActiveFrontend {
                package: FrontendPackage::default_package(),
                is_default: true,
                signature: SignatureCheck::Trusted,
                recovery: None,
            })),
        };
        service.apply_recorded_active();
        service
    }

    /// The active package's summary.
    pub fn active_info(&self) -> ActiveInfo {
        let active = self.active.read().expect("frontend active lock").clone();
        ActiveInfo {
            object: "frontend_package",
            id: active.package.manifest.id.clone(),
            version: active.package.manifest.version.clone(),
            name: active.package.manifest.name.clone(),
            is_default: active.is_default,
            origin: if active.is_default {
                "bundled"
            } else {
                "data-dir"
            },
            page_surfaces: active.package.manifest.page_surfaces.clone(),
            signature: active.signature.as_str(),
            recovery_notice: active.recovery.clone(),
        }
    }

    /// Consume the boot-time recovery notice for auditing (the
    /// notice itself stays visible to `/frontend/active` until the
    /// next activation or reset clears it).
    pub fn take_recovery(&self) -> Option<RecoveryNotice> {
        let guard = self.active.read().expect("frontend active lock");
        guard.recovery.clone()
    }

    /// Route one HTTP path (the text after `/zim/`) through the
    /// active package.
    pub fn resolve(&self, http_path: &str) -> Resolution {
        let active = self.active.read().expect("frontend active lock").clone();
        let path = http_path.trim_matches('/');
        // Exact file match first (assets and anything directly
        // addressed), then the declared surface patterns.
        if !path.is_empty() && safe_path_http(path) {
            if let Some(mime) = mime_of(path) {
                if active.package.file(path).is_some() {
                    return Resolution::File {
                        path: path.to_owned(),
                        mime,
                        cacheable: !path.ends_with(".html"),
                    };
                }
            }
        }
        if let Some((file, _params)) = active.package.resolve_surface(path) {
            if let Some(mime) = mime_of(&file) {
                if active.package.file(&file).is_some() {
                    return Resolution::File {
                        path: file,
                        mime,
                        cacheable: false,
                    };
                }
            }
        }
        Resolution::NotFound
    }

    /// One file's bytes from the active package.
    pub fn file(&self, path: &str) -> Option<Vec<u8>> {
        let active = self.active.read().expect("frontend active lock");
        active.package.file(path).map(<[u8]>::to_vec)
    }

    /// One file's SHA-256 digest (hex), for serving-side ETags.
    pub fn file_digest(&self, path: &str) -> Option<String> {
        use sha2::{Digest, Sha256};
        let bytes = self.file(path)?;
        let digest = Sha256::digest(&bytes);
        Some(digest.iter().map(|b| format!("{b:02x}")).collect())
    }

    /// The active package's declared surfaces (for diagnostics).
    pub fn surfaces(&self) -> Vec<String> {
        self.active
            .read()
            .expect("frontend active lock")
            .package
            .manifest
            .page_surfaces
            .clone()
    }

    /// Store an uploaded package (ZIP bytes; §10.14 upload) without
    /// activating it. Returns its summary plus its gate report.
    pub fn upload(&self, zip_bytes: &[u8]) -> Result<(PackageSummary, SmokeReport), FrontendError> {
        let package = FrontendPackage::from_zip(zip_bytes)?;
        let signature = self.trust.check(&package.manifest);
        if signature.is_hard_reject() {
            // The gate below would also catch it, but failing at
            // upload already prevents junk from being stored.
            let report = smoke::run(
                &package,
                &self.trust,
                &self.engine_version,
                &self.api_version,
            );
            return Err(FrontendError::SmokeTestFailed(Box::new(report)));
        }
        let report = smoke::run(
            &package,
            &self.trust,
            &self.engine_version,
            &self.api_version,
        );
        let summary = PackageSummary::of(&package, &signature);

        let dir = self.package_dir(&package.manifest);
        if dir.exists() {
            return Err(FrontendError::AlreadyExists(format!(
                "{}@{}",
                package.manifest.id, package.manifest.version
            )));
        }
        package
            .write_dir(&dir)
            .map_err(|e| FrontendError::Storage(e.to_string()))?;
        Ok((summary, report))
    }

    /// List the stored (uploaded) packages.
    pub fn stored_packages(&self) -> Vec<String> {
        let mut out = Vec::new();
        let root = self.data_dir.join("packages");
        let Ok(ids) = std::fs::read_dir(&root) else {
            return out;
        };
        for id in ids.flatten() {
            if !id.path().is_dir() {
                continue;
            }
            let Ok(versions) = std::fs::read_dir(id.path()) else {
                continue;
            };
            for version in versions.flatten() {
                if version.path().join(MANIFEST_FILE).is_file() {
                    out.push(format!(
                        "{}/{}",
                        id.file_name().to_string_lossy(),
                        version.file_name().to_string_lossy()
                    ));
                }
            }
        }
        out.sort();
        out
    }

    /// Run the gate on a stored package without activating it.
    pub fn smoke_test(
        &self,
        id: &str,
        version: Option<&str>,
    ) -> Result<SmokeReport, FrontendError> {
        let package = self.load_stored(id, version)?;
        Ok(smoke::run(
            &package,
            &self.trust,
            &self.engine_version,
            &self.api_version,
        ))
    }

    /// Activate a package: the bundled default (`id` of
    /// [`DEFAULT_ID`] or the literal `"default"`), or a stored
    /// upload that must pass the gate. Untrusted packages require
    /// explicit approval.
    pub fn activate(
        &self,
        id: &str,
        version: Option<&str>,
        approve_untrusted: bool,
    ) -> Result<ActiveInfo, FrontendError> {
        if id == "default" || id == DEFAULT_ID {
            return Ok(self.reset());
        }

        let package = self.load_stored(id, version)?;
        let signature = self.trust.check(&package.manifest);
        if signature.is_hard_reject() {
            let report = smoke::run(
                &package,
                &self.trust,
                &self.engine_version,
                &self.api_version,
            );
            return Err(FrontendError::SmokeTestFailed(Box::new(report)));
        }
        let report = smoke::run(
            &package,
            &self.trust,
            &self.engine_version,
            &self.api_version,
        );
        if !report.passed() {
            return Err(FrontendError::SmokeTestFailed(Box::new(report)));
        }
        if signature.requires_approval() && !approve_untrusted {
            return Err(FrontendError::ApprovalRequired(signature.as_str()));
        }

        // Record + swap.
        let record = ActiveRecord {
            id: package.manifest.id.clone(),
            version: Some(package.manifest.version.clone()),
        };
        write_json(&self.data_dir.join("active.json"), &record)
            .map_err(|e| FrontendError::Storage(format!("cannot record active package: {e}")))?;
        let info = ActiveInfo {
            object: "frontend_package",
            id: package.manifest.id.clone(),
            version: package.manifest.version.clone(),
            name: package.manifest.name.clone(),
            is_default: false,
            origin: "data-dir",
            page_surfaces: package.manifest.page_surfaces.clone(),
            signature: signature.as_str(),
            recovery_notice: None,
        };
        *self.active.write().expect("frontend active lock") = Arc::new(ActiveFrontend {
            package,
            is_default: false,
            signature,
            recovery: None,
        });
        Ok(info)
    }

    /// Reset to the bundled default (§10.14 reset).
    pub fn reset(&self) -> ActiveInfo {
        let record = ActiveRecord {
            id: DEFAULT_ID.to_owned(),
            version: None,
        };
        let _ = write_json(&self.data_dir.join("active.json"), &record);
        let info = ActiveInfo {
            object: "frontend_package",
            id: DEFAULT_ID.to_owned(),
            version: "1.0.0".to_owned(),
            name: "ZIM Default Frontend".to_owned(),
            is_default: true,
            origin: "bundled",
            page_surfaces: self
                .active
                .read()
                .expect("frontend active lock")
                .package
                .manifest
                .page_surfaces
                .clone(),
            signature: "trusted",
            recovery_notice: None,
        };
        *self.active.write().expect("frontend active lock") = Arc::new(ActiveFrontend {
            package: FrontendPackage::default_package(),
            is_default: true,
            signature: SignatureCheck::Trusted,
            recovery: None,
        });
        // The reset info's surfaces come from the freshly loaded
        // default, not the previously active package.
        ActiveInfo {
            page_surfaces: self.surfaces(),
            ..info
        }
    }

    /// Load the package directory for an id/version.
    fn package_dir(&self, manifest: &FrontendManifest) -> PathBuf {
        self.data_dir
            .join("packages")
            .join(manifest.id.replace('/', "_"))
            .join(&manifest.version)
    }

    /// Load a stored package by id (+optional version; latest when
    /// `None` — "latest" is the highest stored semver).
    fn load_stored(
        &self,
        id: &str,
        version: Option<&str>,
    ) -> Result<FrontendPackage, FrontendError> {
        let root = self.data_dir.join("packages").join(id.replace('/', "_"));
        if !root.is_dir() {
            return Err(FrontendError::NotFound(id.to_owned()));
        }
        let dir = match version {
            Some(v) => root.join(v),
            None => {
                let mut best: Option<(semver::Version, PathBuf)> = None;
                for entry in std::fs::read_dir(&root)
                    .map_err(|e| FrontendError::Storage(e.to_string()))?
                    .flatten()
                {
                    if !entry.path().is_dir() {
                        continue;
                    }
                    let name = entry.file_name().to_string_lossy().to_string();
                    if let Ok(parsed) = semver::Version::parse(&name) {
                        if best.as_ref().is_none_or(|(v, _)| parsed > *v) {
                            best = Some((parsed, entry.path()));
                        }
                    }
                }
                best.map(|(_, path)| path)
                    .ok_or_else(|| FrontendError::NotFound(id.to_owned()))?
            }
        };
        FrontendPackage::from_dir(&dir).map_err(FrontendError::from)
    }

    /// Apply `{data_dir}/frontend/active.json` at boot, running the
    /// gate — and the §3.6 recovery path — on a custom record.
    fn apply_recorded_active(&self) {
        let record_path = self.data_dir.join("active.json");
        let Ok(bytes) = std::fs::read(&record_path) else {
            // No record yet: persist the default so the data dir
            // always states what is being served.
            let _ = write_json(
                &record_path,
                &ActiveRecord {
                    id: DEFAULT_ID.to_owned(),
                    version: None,
                },
            );
            return;
        };
        let Ok(record) = serde_json::from_slice::<ActiveRecord>(&bytes) else {
            self.recover("active.json is unreadable; keeping the recorded custom id unknown");
            return;
        };
        if record.id == DEFAULT_ID || record.id == "default" {
            return;
        }
        let loaded = FrontendPackage::from_dir(
            &self
                .data_dir
                .join("packages")
                .join(record.id.replace('/', "_"))
                .join(record.version.as_deref().unwrap_or("")),
        );
        let package = match loaded {
            Ok(package) => package,
            Err(error) => {
                self.recover(&format!(
                    "the recorded active package {} failed to load: {error}",
                    record.id
                ));
                return;
            }
        };
        let signature = self.trust.check(&package.manifest);
        let report = smoke::run(
            &package,
            &self.trust,
            &self.engine_version,
            &self.api_version,
        );
        if signature.is_hard_reject() || !report.passed() {
            self.recover(&format!(
                "the recorded active package {} failed its re-verification gate",
                record.id
            ));
            return;
        }
        *self.active.write().expect("frontend active lock") = Arc::new(ActiveFrontend {
            package,
            is_default: false,
            signature,
            recovery: None,
        });
    }

    /// The §3.6 recovery path: fall back to the bundled default,
    /// keep the notice for the next `zim://newtab` load.
    fn recover(&self, reason: &str) {
        eprintln!("zim_engine_frontend_loader: reverting to the default frontend — {reason}");
        *self.active.write().expect("frontend active lock") = Arc::new(ActiveFrontend {
            package: FrontendPackage::default_package(),
            is_default: true,
            signature: SignatureCheck::Trusted,
            recovery: Some(RecoveryNotice {
                reason: reason.to_owned(),
                at: chrono::Utc::now(),
                from: "custom".to_owned(),
            }),
        });
    }
}

/// HTTP-path safety for `/zim/…` serving (same rules as package
/// paths, applied to the URL the caller typed).
fn safe_path_http(path: &str) -> bool {
    !path.contains('\\')
        && !path
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
}

/// The MIME type for a package file, by extension.
pub fn mime_of(path: &str) -> Option<&'static str> {
    let extension = path.rsplit('.').next().unwrap_or("");
    Some(match extension {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "webp" => "image/webp",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" => "text/plain; charset=utf-8",
        _ => return None,
    })
}

/// Load the operator trust file on top of the built-in keys.
fn load_trust_store(frontend_dir: &Path) -> TrustStore {
    let extra = std::fs::read(frontend_dir.join("trusted-keys.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<TrustedKeyFile>(&bytes).ok())
        .map(|file| {
            file.keys
                .into_iter()
                .map(|entry| entry.public_key)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    TrustStore::with_extra_keys(extra.into_iter())
}

/// Best-effort pretty JSON write, creating parent directories.
fn write_json(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signature::{dev_sign, DEV_SIGNING_KEY};

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("zim-frontend-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A complete custom package as upload ZIP bytes.
    fn custom_zip() -> Vec<u8> {
        let mut manifest = crate::manifest::FrontendManifest {
            id: "org.example.frontend.custom".to_owned(),
            manifest_version: 1,
            version: "0.1.0".to_owned(),
            name: "Custom".to_owned(),
            description: "test".to_owned(),
            author: "Example".to_owned(),
            license: "MIT".to_owned(),
            api_version: "v1".to_owned(),
            engines: std::collections::BTreeMap::from([("zim".to_owned(), ">=0.0.1".to_owned())]),
            full_replacement: false,
            smoke_test: Some("pages/smoke.html".to_owned()),
            page_surfaces: vec!["zim://newtab".to_owned()],
            entrypoints: std::collections::BTreeMap::from([(
                "newtab".to_owned(),
                "pages/newtab.html".to_owned(),
            )]),
            assets: vec!["assets/api.js".to_owned()],
            permissions: vec![],
            accessibility: None,
            signature: None,
        };
        manifest.signature = Some(crate::manifest::SignatureInfo {
            algorithm: "ed25519".to_owned(),
            public_key: crate::signature::public_key_of(DEV_SIGNING_KEY),
            value: dev_sign(&manifest),
        });
        let mut buffer = Vec::new();
        {
            use std::io::Write;
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
            let options = zip::write::SimpleFileOptions::default();
            writer.start_file(MANIFEST_FILE, options).unwrap();
            writer
                .write_all(&serde_json::to_vec(&manifest).unwrap())
                .unwrap();
            writer.start_file("pages/newtab.html", options).unwrap();
            writer
                .write_all(
                    br#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><title>x</title>
<script type="module" src="/zim/assets/api.js"></script>
</head><body>custom</body></html>"#,
                )
                .unwrap();
            writer.start_file("pages/smoke.html", options).unwrap();
            writer
                .write_all(
                    br#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><title>s</title>
<script type="module" src="/zim/assets/api.js"></script>
</head><body></body></html>"#,
                )
                .unwrap();
            writer.start_file("assets/api.js", options).unwrap();
            writer
                .write_all(b"export const whoami = () => fetch('/api/v1/users/me');")
                .unwrap();
            writer.finish().unwrap();
        }
        buffer
    }

    #[test]
    fn fresh_engine_serves_the_default() {
        let service =
            FrontendService::new(temp_dir("fresh"), semver::Version::new(0, 0, 1), "v1.0.0");
        let info = service.active_info();
        assert!(info.is_default);
        assert_eq!(info.id, DEFAULT_ID);
        assert!(info.recovery_notice.is_none());
        assert!(matches!(service.resolve("newtab"), Resolution::File { .. }));
        assert!(matches!(service.resolve(""), Resolution::File { .. }));
        assert!(matches!(service.resolve("nope"), Resolution::NotFound));
        assert!(service.file("pages/newtab.html").is_some());
    }

    #[test]
    fn upload_then_activate_serves_the_custom_package() {
        let dir = temp_dir("activate");
        let service = FrontendService::new(dir.clone(), semver::Version::new(0, 0, 1), "v1.0.0");
        let (summary, report) = service.upload(&custom_zip()).unwrap();
        assert_eq!(summary.id, "org.example.frontend.custom");
        assert_eq!(summary.signature, "trusted");
        assert!(report.passed(), "{report}");
        assert_eq!(service.stored_packages().len(), 1);

        // Activate without approval: trusted, so it succeeds.
        let info = service
            .activate("org.example.frontend.custom", Some("0.1.0"), false)
            .unwrap();
        assert!(!info.is_default);
        assert_eq!(info.id, "org.example.frontend.custom");
        assert!(service
            .file("pages/newtab.html")
            .unwrap()
            .windows(6)
            .any(|w| w == b"custom"));
        // The custom surface routes; the default's console does not.
        assert!(matches!(service.resolve("newtab"), Resolution::File { .. }));
        assert!(matches!(service.resolve("servers"), Resolution::NotFound));

        // Reset returns to the default.
        let info = service.reset();
        assert!(info.is_default);
        assert!(matches!(
            service.resolve("servers"),
            Resolution::File { .. }
        ));

        // Re-upload of the same id+version is a conflict.
        assert!(matches!(
            service.upload(&custom_zip()),
            Err(FrontendError::AlreadyExists(_))
        ));
    }

    #[test]
    fn a_failing_package_is_rejected_and_the_default_is_kept() {
        let dir = temp_dir("reject");
        let service = FrontendService::new(dir, semver::Version::new(0, 0, 1), "v1.0.0");
        // Corrupt the otherwise-valid archive: break the HTML.
        let zip = custom_zip();
        // Flip the "<body>" of newtab.html content to create a
        // structural error by rewriting the stored bytes — easier:
        // build a second package with inline script.
        let mut manifest = crate::manifest::FrontendManifest {
            id: "org.example.frontend.broken".to_owned(),
            manifest_version: 1,
            version: "0.1.0".to_owned(),
            name: "Broken".to_owned(),
            description: String::new(),
            author: "Example".to_owned(),
            license: "MIT".to_owned(),
            api_version: "v1".to_owned(),
            engines: std::collections::BTreeMap::from([("zim".to_owned(), ">=0.0.1".to_owned())]),
            full_replacement: false,
            smoke_test: Some("pages/smoke.html".to_owned()),
            page_surfaces: vec!["zim://newtab".to_owned()],
            entrypoints: std::collections::BTreeMap::from([(
                "newtab".to_owned(),
                "pages/newtab.html".to_owned(),
            )]),
            assets: vec!["assets/api.js".to_owned()],
            permissions: vec![],
            accessibility: None,
            signature: None,
        };
        manifest.signature = Some(crate::manifest::SignatureInfo {
            algorithm: "ed25519".to_owned(),
            public_key: crate::signature::public_key_of(DEV_SIGNING_KEY),
            value: dev_sign(&manifest),
        });
        let mut buffer = Vec::new();
        {
            use std::io::Write;
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
            let options = zip::write::SimpleFileOptions::default();
            writer.start_file(MANIFEST_FILE, options).unwrap();
            writer
                .write_all(&serde_json::to_vec(&manifest).unwrap())
                .unwrap();
            writer.start_file("pages/newtab.html", options).unwrap();
            writer
                .write_all(br#"<html><head></head><body><script>inline()</script></body></html>"#)
                .unwrap();
            writer.start_file("pages/smoke.html", options).unwrap();
            writer
                .write_all(br#"<html><head></head><body></body></html>"#)
                .unwrap();
            writer.start_file("assets/api.js", options).unwrap();
            writer
                .write_all(b"export const whoami = () => fetch('/api/v1/users/me');")
                .unwrap();
            writer.finish().unwrap();
        }

        let (summary, report) = service.upload(&buffer).unwrap();
        assert_eq!(summary.id, "org.example.frontend.broken");
        assert!(!report.passed(), "the inline-script package must fail");
        let info = service.active_info();
        assert!(info.is_default, "the default must stay active");

        // Activation of the failing package is refused.
        let error = service
            .activate("org.example.frontend.broken", None, true)
            .unwrap_err();
        assert!(matches!(error, FrontendError::SmokeTestFailed(_)));
        assert!(service.active_info().is_default);
        drop(zip);
    }

    #[test]
    fn unsigned_packages_need_explicit_approval() {
        let dir = temp_dir("approval");
        let service = FrontendService::new(dir, semver::Version::new(0, 0, 1), "v1.0.0");
        // Same as custom_zip but with the signature stripped.
        let mut bytes = custom_zip();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
        let mut manifest_text = String::new();
        {
            use std::io::Read;
            archive
                .by_name(MANIFEST_FILE)
                .unwrap()
                .read_to_string(&mut manifest_text)
                .unwrap();
        }
        let mut manifest: serde_json::Value = serde_json::from_str(&manifest_text).unwrap();
        manifest["signature"] = serde_json::Value::Null;
        let mut rebuilt = Vec::new();
        {
            use std::io::{Read, Write};
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut rebuilt));
            let options = zip::write::SimpleFileOptions::default();
            writer.start_file(MANIFEST_FILE, options).unwrap();
            writer
                .write_all(serde_json::to_vec(&manifest).unwrap().as_slice())
                .unwrap();
            for name in ["pages/newtab.html", "pages/smoke.html", "assets/api.js"] {
                writer.start_file(name, options).unwrap();
                let mut content = Vec::new();
                archive
                    .by_name(name)
                    .unwrap()
                    .read_to_end(&mut content)
                    .unwrap();
                writer.write_all(&content).unwrap();
            }
            writer.finish().unwrap();
        }
        bytes = rebuilt;

        service.upload(&bytes).unwrap();
        let error = service
            .activate("org.example.frontend.custom", Some("0.1.0"), false)
            .unwrap_err();
        assert!(matches!(error, FrontendError::ApprovalRequired("unsigned")));

        // With approval, activation succeeds.
        let info = service
            .activate("org.example.frontend.custom", Some("0.1.0"), true)
            .unwrap();
        assert!(!info.is_default);
    }

    #[test]
    fn boot_with_a_corrupted_active_record_recovers_to_default() {
        let dir = temp_dir("recovery");
        // First boot: upload + activate the custom package.
        {
            let service = FrontendService::new(&dir, semver::Version::new(0, 0, 1), "v1.0.0");
            service.upload(&custom_zip()).unwrap();
            service
                .activate("org.example.frontend.custom", None, false)
                .unwrap();
        }
        // Corrupt the extracted package on disk.
        let package_dir = dir
            .join("frontend")
            .join("packages")
            .join("org.example.frontend.custom")
            .join("0.1.0");
        std::fs::remove_file(package_dir.join("pages").join("newtab.html")).unwrap();
        // Second boot: the record points at a broken package.
        let service = FrontendService::new(&dir, semver::Version::new(0, 0, 1), "v1.0.0");
        let info = service.active_info();
        assert!(info.is_default, "the engine must revert to the default");
        let notice = info.recovery_notice.expect("a recovery notice is surfaced");
        assert!(notice.reason.contains("failed"));
        assert!(service.take_recovery().is_some());
        // The default still serves.
        assert!(matches!(service.resolve("newtab"), Resolution::File { .. }));
    }

    #[test]
    fn boot_with_a_failing_active_package_recovers_to_default() {
        let dir = temp_dir("recovery-gate");
        // Upload a package that passes storage validation but would
        // fail the gate at boot: build it with an entrypoint whose
        // file is deleted AFTER activation (activation passed because
        // the gate ran on the in-memory copy... no — the gate reads
        // the same files. Simulate instead: activate a good package,
        // then swap the stored manifest's engines field on disk).
        {
            let service = FrontendService::new(&dir, semver::Version::new(0, 0, 1), "v1.0.0");
            service.upload(&custom_zip()).unwrap();
            service
                .activate("org.example.frontend.custom", None, false)
                .unwrap();
        }
        let manifest_path = dir
            .join("frontend")
            .join("packages")
            .join("org.example.frontend.custom")
            .join("0.1.0")
            .join(MANIFEST_FILE);
        let mut manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        manifest["engines"]["zim"] = serde_json::json!(">=99.0.0");
        std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();

        let service = FrontendService::new(&dir, semver::Version::new(0, 0, 1), "v1.0.0");
        let info = service.active_info();
        assert!(info.is_default);
        assert!(info.recovery_notice.is_some());
    }

    #[test]
    fn http_paths_are_confined() {
        let service =
            FrontendService::new(temp_dir("paths"), semver::Version::new(0, 0, 1), "v1.0.0");
        assert!(matches!(
            service.resolve("../engine.db"),
            Resolution::NotFound
        ));
        assert!(matches!(service.resolve("a//b"), Resolution::NotFound));
        assert!(matches!(
            service.resolve("assets\\main.css"),
            Resolution::NotFound
        ));
        // A real asset resolves.
        assert!(matches!(
            service.resolve("assets/main.css"),
            Resolution::File {
                cacheable: true,
                ..
            }
        ));
    }
}
