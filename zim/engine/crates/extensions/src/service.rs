//! The addon service: the engine's extension loader (M24).
//!
//! Owns the installed-addon registry (the `extension_registrations`
//! rows, written through the `Persistence` port), the extracted
//! package trees on disk, the install/enable/disable/uninstall/grant
//! lifecycle (§4.11), dependency resolution (§4.9), page-surface
//! collision detection (§4.4), error isolation (§4.10 — an addon
//! whose package is corrupt on disk is disabled with a notice and
//! everything else keeps working), and the `/zim/` fallback routing
//! that serves enabled addons' pages.
//!
//! # Layout in the data directory
//!
//! ```text
//! {data_dir}/extensions/
//!   trusted-keys.json              # operator-added verifying keys
//!   packages/{id}/{version}/       # extracted package trees
//! ```
//!
//! # Sources of truth
//!
//! - SQLite (`extension_registrations`) is the durable registration
//!   state: enabled, granted permissions, installed version, the
//!   manifest snapshot.
//! - The extracted tree on disk is the serving payload; its
//!   `zim.addon.json` is re-validated on every load — a corrupt one
//!   disables the addon (§4.10), never the engine.
//! - The in-memory index (`RwLock<BTreeMap<…>>`) is the serving
//!   cache, rebuilt after every lifecycle mutation and at boot.
//!
//! # Concurrency
//!
//! Lifecycle mutations run inside the service's `Mutex<()>` (admin
//! operations are rare); page serving reads the index behind the
//! `RwLock` and never blocks on mutations beyond the swap.
//!
//! # Delivery notes (documented scope decisions)
//!
//! - §4.10's process isolation language predates AD-20 (the Chromium
//!   import cancellation). In ZIM's post-AD-20 serving model every
//!   page — the default frontend's and addons' — is same-origin on
//!   the engine's loopback HTTPS origin, so "an addon that crashes
//!   its renderer" reads as "an addon whose page fails" and the
//!   engine's isolation answer is disable-with-notice plus the
//!   frontend's per-pane iframe boundaries (AD-24). This is recorded
//!   as AD-29.
//! - §4.2's "checked at the engine boundary on every API call":
//!   addon pages call the same `/api/v1` surface as the default
//!   frontend's pages, with the same session credentials — the
//!   engine's per-endpoint authz (invariant 15) gates every call.
//!   The manifest's declared permission set is granted, listed, and
//!   revocable (§4.2's admin-approval elevation path), and the SDK's
//!   client refuses calls outside the addon's grants; server-side
//!   per-addon attribution on the shared loopback origin is an M25
//!   hardening item (recorded with the roadmap's honest-boundary
//!   notes).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use serde::Serialize;
use sha2::{Digest, Sha256};
use zim_engine_application::ports::Persistence;
use zim_engine_domain::{
    ExtensionId, ExtensionManifest, ExtensionRegistration, ExtensionSignature, InstanceScope as
    DomainInstanceScope, PermissionGrant, PermissionSet, SignatureScheme,
};
use zim_engine_frontend_loader::signature::SignatureCheck;
use zim_engine_frontend_loader::{FrontendService, TrustStore};
use zim_engine_frontend_loader::service::mime_of;
use zim_engine_frontend_loader::manifest::SurfacePattern;

use crate::manifest::{AddonManifest, ManifestError, MANIFEST_FILE};
use crate::package::{AddonPackage, PackageError};
use crate::signature;

/// Everything the addon subsystem can fail with; the API layer maps
/// these onto the §6.1 vocabulary.
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
pub enum ExtensionError {
    /// The upload archive is unusable.
    #[error("{0}")]
    BadPackage(#[from] PackageError),
    /// The manifest failed validation.
    #[error("{0}")]
    BadManifest(#[from] ManifestError),
    /// The manifest speaks an API generation this engine does not.
    #[error("the addon speaks api_version {0}, this engine speaks {1}")]
    ApiGeneration(String, String),
    /// The manifest's engine range does not match this engine.
    #[error("the addon requires zim {0}, this engine is {1}")]
    EngineIncompatible(String, String),
    /// The signature failed against a trusted key — no approval can
    /// override this (§4.11).
    #[error("the addon's signature is invalid (signed by a trusted key but it does not verify)")]
    BadSignature,
    /// No addon with that manifest id is installed.
    #[error("no addon {0}")]
    NotFound(String),
    /// The same id+version is already installed.
    #[error("addon {0} version {1} is already installed")]
    AlreadyExists(String, String),
    /// An update to a lower version was attempted.
    #[error("addon {0} is at {1}; refusing downgrade to {2}")]
    DowngradeRefused(String, String, String),
    /// The declared page surfaces collide with an existing package.
    #[error("page surface {surface} collides with {owner}")]
    SurfaceCollision {
        /// The colliding surface.
        surface: String,
        /// Who already owns it.
        owner: String,
    },
    /// A dependency is not satisfiable (§4.9 — no partial install).
    #[error("dependency {0} is not satisfied by any installed package")]
    DependencyUnsatisfied(String),
    /// Another installed addon depends on this one.
    #[error("addon {0} is required by {1}")]
    HasDependents(String, String),
    /// Bundled default addons are disable-only (§6).
    #[error("{0} is a bundled default addon: it can be disabled, not removed")]
    BundledNotRemovable(String),
    /// A data-directory failure.
    #[error("addon storage error: {0}")]
    Storage(String),
    /// A persistence failure.
    #[error("persistence error: {0}")]
    Persistence(#[from] zim_engine_application::ports::PersistenceError),
    /// A permission name outside the vocabulary.
    #[error("{0} is not in the engine's permission vocabulary")]
    UnknownPermission(String),
}

/// One addon loaded into the serving index.
#[derive(Debug)]
pub struct LoadedAddon {
    /// The re-validated manifest (from the extracted tree).
    pub manifest: AddonManifest,
    /// Parsed surface patterns (the manifest's `page_surfaces`).
    pub surfaces: Vec<SurfacePattern>,
    /// The extracted package tree: path → bytes.
    pub files: BTreeMap<String, Vec<u8>>,
    /// The durable registration row (uuid, enabled, granted set…).
    pub registration: ExtensionRegistration,
    /// Whether this is a bundled default (§6: disable-only).
    pub bundled: bool,
    /// The signature classification computed at load.
    pub signature_class: SignatureCheck,
    /// A §4.10 notice (why the addon is inert), if any.
    pub notice: Option<String>,
}

/// A file resolved out of an enabled addon for `/zim/` serving.
#[derive(Debug, Clone)]
pub struct ServedFile {
    /// The file's bytes.
    pub bytes: Vec<u8>,
    /// The content type.
    pub mime: &'static str,
    /// The sha256 content digest (hex) — the ETag source.
    pub digest: String,
    /// The serving addon's manifest id.
    pub addon_id: String,
}

/// The list/install summary shape (§10.12 responses).
#[derive(Debug, Clone, Serialize)]
pub struct AddonSummary {
    /// The manifest id (reverse-DNS) — the URL-facing identifier.
    pub id: String,
    /// The installed version.
    pub version: String,
    /// Human name.
    pub name: String,
    /// Human description.
    pub description: String,
    /// Author.
    pub author: String,
    /// License.
    pub license: String,
    /// Whether the addon is enabled.
    pub enabled: bool,
    /// Whether this is a bundled default (§6).
    pub bundled: bool,
    /// The instance-scope declaration (§4.3).
    pub instance_scope: String,
    /// Requested vs. granted permission names (§4.2).
    pub permissions: PermissionSummary,
    /// The signature classification.
    pub signature: &'static str,
    /// When the addon was installed.
    pub installed_at: chrono::DateTime<chrono::Utc>,
    /// The contributed `zim://` surfaces (§4.4).
    pub page_surfaces: Vec<String>,
    /// Sidebar contributions (§4.5).
    pub sidebar_items: Vec<crate::manifest::SidebarItem>,
    /// Context-menu contributions (§4.6).
    pub context_menu_items: Vec<crate::manifest::ContextMenuItem>,
    /// Commands (§4.7).
    pub commands: Vec<crate::manifest::CommandSpec>,
    /// Event subscriptions (§4.8).
    pub event_subscriptions: Vec<String>,
    /// Dependency specifiers (§4.9).
    pub dependencies: Vec<String>,
    /// The §4.10 notice, when the addon is inert.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

/// The requested-vs-granted permission split.
#[derive(Debug, Clone, Serialize)]
pub struct PermissionSummary {
    /// What the manifest declares.
    pub requested: Vec<String>,
    /// What the admin has granted (⊇ requested after §4.2 grants).
    pub granted: Vec<String>,
}

/// The service.
pub struct ExtensionsService {
    persistence: Arc<dyn Persistence>,
    frontend: Arc<FrontendService>,
    data_dir: PathBuf,
    engine_version: semver::Version,
    api_version: String,
    trust: TrustStore,
    addons: RwLock<BTreeMap<String, Arc<LoadedAddon>>>,
    lifecycle: Mutex<()>,
    boot_notices: Mutex<Vec<String>>,
}

impl ExtensionsService {
    /// Construct + boot: reconcile the registry against disk, seed
    /// the bundled defaults on a fresh data dir, and build the
    /// serving index. Failures degrade to per-addon notices (§4.10)
    /// — a broken addon can never lock the engine out, mirroring the
    /// frontend loader's invariant-16 posture.
    pub fn new(
        persistence: Arc<dyn Persistence>,
        frontend: Arc<FrontendService>,
        data_dir: &Path,
        engine_version: semver::Version,
        api_version: &str,
    ) -> Self {
        Self::with_bundled(
            persistence,
            frontend,
            data_dir,
            engine_version,
            api_version,
            true,
        )
    }

    /// The full constructor: `seed_bundled: false` skips the §6
    /// bundled-default seeding (the unit tests' empty-registry
    /// fixture; the dedicated seeding test uses `true`).
    pub fn with_bundled(
        persistence: Arc<dyn Persistence>,
        frontend: Arc<FrontendService>,
        data_dir: &Path,
        engine_version: semver::Version,
        api_version: &str,
        seed_bundled: bool,
    ) -> Self {
        let service = Self {
            persistence,
            frontend,
            data_dir: data_dir.to_owned(),
            engine_version,
            api_version: api_version.to_owned(),
            trust: signature::trust_store(data_dir),
            addons: RwLock::new(BTreeMap::new()),
            lifecycle: Mutex::new(()),
            boot_notices: Mutex::new(Vec::new()),
        };

        // Seed the bundled defaults (org.zaminmc.extension.* — §6):
        // registered on a fresh data dir, re-extracted if their tree
        // went missing, never removed by the operator.
        if seed_bundled {
            for (bundled_id, files) in crate::embedded::bundled_packages() {
                if let Err(message) = service.seed_bundled(&bundled_id, &files) {
                    service
                        .boot_notices
                        .lock()
                        .expect("boot notices lock")
                        .push(format!("bundled addon {bundled_id}: {message}"));
                }
            }
        }

        if let Err(message) = service.rebuild_index() {
            service
                .boot_notices
                .lock()
                .expect("boot notices lock")
                .push(format!("registry load: {message}"));
        }
        service
    }

    /// The `{data_dir}/extensions` root.
    fn root(&self) -> PathBuf {
        self.data_dir.join("extensions")
    }

    /// One addon's extracted tree.
    fn package_dir(&self, manifest_id: &str, version: &str) -> PathBuf {
        self.root()
            .join("packages")
            .join(manifest_id)
            .join(version)
    }

    /// `GET /api/v1/extensions` — every installed addon, bundled
    /// first, then by id.
    pub fn list(&self) -> Vec<AddonSummary> {
        let addons = self
            .addons
            .read()
            .expect("addon index lock")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut summaries: Vec<AddonSummary> = addons.iter().map(|a| a.summary()).collect();
        summaries.sort_by(|a, b| {
            b.bundled
                .cmp(&a.bundled)
                .then_with(|| a.id.cmp(&b.id))
        });
        summaries
    }

    /// Boot-time notices (seed/reload failures) — surfaced by the
    /// API layer alongside the list.
    pub fn boot_notices(&self) -> Vec<String> {
        self.boot_notices
            .lock()
            .expect("boot notices lock")
            .clone()
    }

    /// `POST /api/v1/extensions` — install (or update) from ZIP
    /// bytes (§4.11: the admin's upload IS the approval for
    /// untrusted/unsigned packages; a trusted-key failure is a hard
    /// reject).
    pub fn install(&self, bytes: &[u8]) -> Result<AddonSummary, ExtensionError> {
        let _guard = self.lifecycle.lock().expect("lifecycle lock");
        let package = AddonPackage::from_zip(bytes)?;
        self.install_package(package)
    }

    /// The shared install path (uploads and bundled seeds).
    fn install_package(&self, package: AddonPackage) -> Result<AddonSummary, ExtensionError> {
        let manifest = package.manifest;

        // Generation + engine compatibility. The API-generation
        // check follows the frontend loader's smoke-gate discipline:
        // major-version equality ("v1" == "v1.0.0"), never exact
        // strings.
        let manifest_major = api_major(&manifest.api_version);
        let engine_major = api_major(&self.api_version);
        if manifest_major != engine_major || manifest_major == "0" {
            return Err(ExtensionError::ApiGeneration(
                manifest.api_version.clone(),
                self.api_version.clone(),
            ));
        }
        if !manifest
            .zim_requirement()
            .matches(&self.engine_version)
        {
            return Err(ExtensionError::EngineIncompatible(
                manifest
                    .engines
                    .get("zim")
                    .cloned()
                    .unwrap_or_else(|| ">=0.0.0".to_owned()),
                self.engine_version.to_string(),
            ));
        }

        // Signature: a trusted key that fails to verify is a hard
        // reject (§4.11); untrusted/unsigned proceed under the
        // admin-approval upload path.
        if signature::classify(&manifest, &self.trust).is_hard_reject() {
            return Err(ExtensionError::BadSignature);
        }

        // Dependencies (§4.9): every specifier must be satisfied by
        // an installed addon or the active frontend package; an
        // unsatisfiable one fails the install with nothing written.
        let index = self
            .addons
            .read()
            .expect("addon index lock")
            .clone();
        for dependency in &manifest.dependencies {
            let (dep_id, range) = crate::manifest::parse_dependency(dependency)
                .map_err(|message| ManifestError::Missing {
                    field: "dependencies",
                    message,
                })?;
            let satisfied = if dep_id == self.frontend.active_info().id {
                let info = self.frontend.active_info();
                semver::Version::parse(&info.version)
                    .map(|v| range.matches(&v))
                    .unwrap_or(false)
            } else {
                index
                    .get(dep_id)
                    .map(|addon| range.matches(&addon.manifest.parsed_version()))
                    .unwrap_or(false)
            };
            if !satisfied {
                return Err(ExtensionError::DependencyUnsatisfied(dependency.clone()));
            }
        }

        // Page-surface collisions (§4.4): against the active
        // frontend's surfaces and every installed addon's — enabled
        // or not (a disabled addon still owns its surfaces until
        // uninstalled).
        let frontend_surfaces: Vec<SurfacePattern> = self
            .frontend
            .active_info()
            .page_surfaces
            .iter()
            .filter_map(|s| SurfacePattern::parse(s).ok())
            .collect();
        for surface in &manifest.page_surfaces {
            let candidate = SurfacePattern::parse(surface)
                .map_err(|message| ManifestError::Missing {
                    field: "page_surfaces",
                    message,
                })?;
            // The engine's own addon-asset namespace is reserved: an
            // addon surface under zim://ext/ would shadow every
            // addon's asset URLs.
            if candidate
                .as_str()
                .trim_start_matches("zim://")
                .starts_with("ext/")
            {
                return Err(ExtensionError::SurfaceCollision {
                    surface: surface.clone(),
                    owner: "the engine's reserved addon-asset namespace (zim://ext/)".to_owned(),
                });
            }
            for existing in &frontend_surfaces {
                if candidate.overlaps(existing) {
                    return Err(ExtensionError::SurfaceCollision {
                        surface: surface.clone(),
                        owner: "the active frontend package".to_owned(),
                    });
                }
            }
            for (other_id, addon) in &index {
                if *other_id == manifest.id {
                    continue;
                }
                for existing in &addon.surfaces {
                    if candidate.overlaps(existing) {
                        return Err(ExtensionError::SurfaceCollision {
                            surface: surface.clone(),
                            owner: format!("addon {other_id}"),
                        });
                    }
                }
            }
        }

        // Update discipline: same version → already installed; lower
        // → downgrade refused (§4.11's downgrade-safety).
        let existing = index.get(&manifest.id);
        if let Some(existing) = existing {
            let installed = existing.manifest.parsed_version();
            let incoming = manifest.parsed_version();
            if incoming == installed {
                return Err(ExtensionError::AlreadyExists(
                    manifest.id.clone(),
                    manifest.version.clone(),
                ));
            }
            if incoming < installed {
                return Err(ExtensionError::DowngradeRefused(
                    manifest.id.clone(),
                    existing.manifest.version.clone(),
                    manifest.version.clone(),
                ));
            }
        }

        // Extract to the package tree (temp dir + rename).
        self.extract(&manifest, package.files())?;

        // Persist the registration (same uuid across updates).
        let registration = self.registration_for(&manifest, existing)?;
        self.persistence.put_extension(&registration)?;

        // Prune superseded version directories.
        if let Some(existing) = existing {
            let old_dir = self.package_dir(&manifest.id, &existing.manifest.version);
            if old_dir.exists() {
                let _ = std::fs::remove_dir_all(&old_dir);
            }
        }

        self.reload_one(&manifest.id);
        self.find(&manifest.id)
            .map(|addon| addon.summary())
            .map_err(|_| ExtensionError::Storage("the addon vanished after install".to_owned()))
    }

    /// Write the package tree to disk (temp dir + rename).
    fn extract(
        &self,
        manifest: &AddonManifest,
        files: &BTreeMap<String, Vec<u8>>,
    ) -> Result<(), ExtensionError> {
        let target = self.package_dir(&manifest.id, &manifest.version);
        let parent = target
            .parent()
            .ok_or_else(|| ExtensionError::Storage("bad package path".to_owned()))?
            .to_owned();
        std::fs::create_dir_all(&parent)
            .map_err(|e| ExtensionError::Storage(format!("creating {}: {e}", parent.display())))?;
        let temp = parent.join(format!(
            ".incoming-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&temp)
            .map_err(|e| ExtensionError::Storage(format!("creating {}: {e}", temp.display())))?;
        for (relative, bytes) in files {
            let file = temp.join(relative);
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir).map_err(|e| {
                    ExtensionError::Storage(format!("creating {}: {e}", dir.display()))
                })?;
            }
            std::fs::write(&file, bytes).map_err(|e| {
                ExtensionError::Storage(format!("writing {}: {e}", file.display()))
            })?;
        }
        // A stale incoming dir (crashed install) is replaced.
        if target.exists() {
            let _ = std::fs::remove_dir_all(&target);
        }
        std::fs::rename(&temp, &target).map_err(|e| {
            ExtensionError::Storage(format!(
                "moving {} to {}: {e}",
                temp.display(),
                target.display()
            ))
        })?;
        Ok(())
    }

    /// Build the durable registration row from a validated manifest.
    fn registration_for(
        &self,
        manifest: &AddonManifest,
        existing: Option<&Arc<LoadedAddon>>,
    ) -> Result<ExtensionRegistration, ExtensionError> {
        Ok(ExtensionRegistration {
            id: existing
                .map(|addon| addon.registration.id)
                .unwrap_or_else(ExtensionId::new),
            manifest: domain_manifest_of(manifest),
            version: manifest.version.clone(),
            installed_at: chrono::Utc::now(),
            enabled: true,
            granted_permissions: permissions_by_name(&manifest.permissions)?,
            signature: manifest.signature.as_ref().map(|block| ExtensionSignature {
                scheme: SignatureScheme::Ed25519,
                key_id: block.public_key.clone(),
                signature: block.value.clone(),
            }),
        })
    }

    /// `POST /api/v1/extensions/{id}/enable`.
    pub fn enable(&self, manifest_id: &str) -> Result<AddonSummary, ExtensionError> {
        self.set_enabled(manifest_id, true)
    }

    /// `POST /api/v1/extensions/{id}/disable`.
    pub fn disable(&self, manifest_id: &str) -> Result<AddonSummary, ExtensionError> {
        self.set_enabled(manifest_id, false)
    }

    fn set_enabled(&self, manifest_id: &str, enabled: bool) -> Result<AddonSummary, ExtensionError> {
        let _guard = self.lifecycle.lock().expect("lifecycle lock");
        let addon = self.find(manifest_id)?;
        let mut registration = addon.registration.clone();
        registration.enabled = enabled;
        self.persistence.put_extension(&registration)?;
        self.reload_one(manifest_id);
        self.find(manifest_id).map(|addon| addon.summary())
    }

    /// `POST /api/v1/extensions/{id}/grant` — grant additional
    /// permissions (§4.2: elevation requires admin re-approval; this
    /// endpoint IS that approval).
    pub fn grant(
        &self,
        manifest_id: &str,
        permissions: &[String],
    ) -> Result<AddonSummary, ExtensionError> {
        let _guard = self.lifecycle.lock().expect("lifecycle lock");
        let addon = self.find(manifest_id)?;
        let additions = permissions_by_name(permissions)?;
        let mut registration = addon.registration.clone();
        for grant in additions.iter() {
            registration.granted_permissions.grant(grant);
        }
        self.persistence.put_extension(&registration)?;
        self.reload_one(manifest_id);
        self.find(manifest_id).map(|addon| addon.summary())
    }

    /// `DELETE /api/v1/extensions/{id}` — uninstall + revoke (§4.11).
    pub fn uninstall(&self, manifest_id: &str) -> Result<(), ExtensionError> {
        let _guard = self.lifecycle.lock().expect("lifecycle lock");
        let addon = self.find(manifest_id)?;
        if addon.bundled {
            return Err(ExtensionError::BundledNotRemovable(manifest_id.to_owned()));
        }
        // Refuse when other installed addons depend on this one —
        // cascade removal would break them silently; the operator
        // uninstalls the dependents first.
        let index = self.addons.read().expect("addon index lock").clone();
        for (other_id, other) in &index {
            if other_id == &addon.manifest.id {
                continue;
            }
            let depends = other.manifest.dependencies.iter().any(|dep| {
                crate::manifest::parse_dependency(dep)
                    .map(|(dep_id, _)| dep_id == addon.manifest.id)
                    .unwrap_or(false)
            });
            if depends {
                return Err(ExtensionError::HasDependents(
                    manifest_id.to_owned(),
                    other_id.clone(),
                ));
            }
        }
        self.persistence
            .delete_extension(&addon.registration.id)?;
        let versions_root = self
            .root()
            .join("packages")
            .join(manifest_id);
        if versions_root.exists() {
            let _ = std::fs::remove_dir_all(&versions_root);
        }
        self.addons
            .write()
            .expect("addon index lock")
            .remove(manifest_id);
        Ok(())
    }

    /// The `/zim/` fallback: resolve a concrete HTTP path (the
    /// `zim://` URL minus scheme and host) against every enabled
    /// addon — first the reserved addon-asset namespace
    /// (`ext/{addon-id}/{file…}`, the `/zim/ext/{id}/assets/…` URLs
    /// addon pages reference), then the declared page surfaces
    /// (§4.4 routing contract — same segment/parameter matching as
    /// the frontend loader's).
    pub fn resolve(&self, path: &str) -> Option<ServedFile> {
        // The reserved asset namespace: ext/{addon-id}/{file…}.
        if let Some(rest) = path.strip_prefix("ext/") {
            let (addon_id, file) = rest.split_once('/')?;
            let addons = self.addons.read().expect("addon index lock");
            let addon = addons.get(addon_id)?;
            if !addon.registration.enabled {
                return None;
            }
            let bytes = addon.files.get(file)?;
            let mime = mime_of(file)?;
            return Some(ServedFile {
                bytes: bytes.clone(),
                mime,
                digest: digest_of(bytes),
                addon_id: addon.manifest.id.clone(),
            });
        }

        let addons = self
            .addons
            .read()
            .expect("addon index lock")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for addon in addons {
            if !addon.registration.enabled {
                continue;
            }
            for pattern in &addon.surfaces {
                if let Some(_params) = pattern.matches_path(path) {
                    let key = pattern.entrypoint_key();
                    let Some(file) = addon.manifest.entrypoints.get(&key) else {
                        continue;
                    };
                    let Some(bytes) = addon.files.get(file) else {
                        continue;
                    };
                    let Some(mime) = mime_of(file) else {
                        continue;
                    };
                    return Some(ServedFile {
                        bytes: bytes.clone(),
                        mime,
                        digest: digest_of(bytes),
                        addon_id: addon.manifest.id.clone(),
                    });
                }
            }
        }
        None
    }

    /// The lookup used by lifecycle endpoints.
    fn find(&self, manifest_id: &str) -> Result<Arc<LoadedAddon>, ExtensionError> {
        self.addons
            .read()
            .expect("addon index lock")
            .get(manifest_id)
            .cloned()
            .ok_or_else(|| ExtensionError::NotFound(manifest_id.to_owned()))
    }

    /// Reload one addon's index entry from disk + DB.
    fn reload_one(&self, manifest_id: &str) {
        let registrations = match self.persistence.list_extensions() {
            Ok(rows) => rows,
            Err(_) => return,
        };
        let Some(row) = registrations
            .iter()
            .find(|row| row.manifest.id == manifest_id)
        else {
            return;
        };
        if let Some(loaded) = self.load_addon(row) {
            self.addons
                .write()
                .expect("addon index lock")
                .insert(manifest_id.to_owned(), Arc::new(loaded));
        } else {
            self.addons
                .write()
                .expect("addon index lock")
                .remove(manifest_id);
        }
    }

    /// Rebuild the whole serving index from the DB + disk.
    fn rebuild_index(&self) -> Result<(), String> {
        let registrations = self
            .persistence
            .list_extensions()
            .map_err(|e| e.to_string())?;
        let mut index: BTreeMap<String, Arc<LoadedAddon>> = BTreeMap::new();
        for row in registrations {
            let manifest_id = row.manifest.id.clone();
            match self.load_addon(&row) {
                Some(loaded) => {
                    index.insert(manifest_id, Arc::new(loaded));
                }
                None => {
                    // §4.10: the package on disk is unusable — disable
                    // the registration and keep everyone else
                    // working.
                    let mut disabled = row.clone();
                    let reason = "the package on disk failed to load (corrupt or missing)";
                    if disabled.enabled {
                        disabled.enabled = false;
                        let _ = self.persistence.put_extension(&disabled);
                    }
                    let manifest = rehydrate_manifest(&disabled)
                        .unwrap_or_else(|| placeholder_manifest(&manifest_id));
                    let surfaces = manifest
                        .page_surfaces
                        .iter()
                        .filter_map(|s| SurfacePattern::parse(s).ok())
                        .collect();
                    index.insert(
                        manifest_id.clone(),
                        Arc::new(LoadedAddon {
                            manifest,
                            surfaces,
                            files: BTreeMap::new(),
                            registration: disabled,
                            bundled: manifest_id.starts_with("org.zaminmc.extension."),
                            signature_class: SignatureCheck::Unsigned,
                            notice: Some(reason.to_owned()),
                        }),
                    );
                }
            }
        }
        *self.addons.write().expect("addon index lock") = index;
        Ok(())
    }

    /// Load one addon's package tree from disk and re-validate its
    /// manifest (§4.10's gate).
    fn load_addon(&self, row: &ExtensionRegistration) -> Option<LoadedAddon> {
        let dir = self.package_dir(&row.manifest.id, &row.version);
        let manifest_path = dir.join(MANIFEST_FILE);
        let manifest_bytes = std::fs::read(&manifest_path).ok()?;
        let manifest = AddonManifest::from_bytes(&manifest_bytes).ok()?;
        let files = load_tree(&dir)?;
        let surfaces = manifest
            .page_surfaces
            .iter()
            .filter_map(|s| SurfacePattern::parse(s).ok())
            .collect();
        Some(LoadedAddon {
            signature_class: signature::classify(&manifest, &self.trust),
            manifest,
            surfaces,
            files,
            bundled: row.manifest.id.starts_with("org.zaminmc.extension."),
            registration: row.clone(),
            notice: None,
        })
    }

    /// Seed (or repair) one bundled default addon's registration and
    /// tree (§6). Idempotent: an existing registration is kept (the
    /// operator's enabled/disabled choice survives reboots), a
    /// missing tree is re-extracted from the embedded bytes.
    fn seed_bundled(
        &self,
        manifest_id: &str,
        files: &BTreeMap<String, Vec<u8>>,
    ) -> Result<(), String> {
        let registrations = self
            .persistence
            .list_extensions()
            .map_err(|e| e.to_string())?;
        let existing = registrations
            .iter()
            .find(|row| row.manifest.id == manifest_id)
            .cloned();

        // Parse + validate the embedded manifest.
        let manifest_bytes = files
            .get(MANIFEST_FILE)
            .ok_or_else(|| format!("no {MANIFEST_FILE} in the embedded bundle"))?;
        let manifest = AddonManifest::from_bytes(manifest_bytes)
            .map_err(|e| format!("embedded manifest: {e}"))?;

        // (Re-)extract when the tree is missing or stale.
        let dir = self.package_dir(manifest_id, &manifest.version);
        let manifest_on_disk = dir.join(MANIFEST_FILE);
        let needs_extract = !manifest_on_disk.exists()
            || std::fs::read(&manifest_on_disk)
                .map(|bytes| bytes != manifest_bytes.as_slice())
                .unwrap_or(true);
        if needs_extract {
            self.extract(&manifest, files)
                .map_err(|e| e.to_string())?;
        }

        match existing {
            Some(row) => {
                // Repair a version bump in the embedded bundle (the
                // engine ships a newer default than the data dir
                // recorded): register the new version, prune the old
                // tree. Never silently flip the enabled flag.
                if row.version != manifest.version
                    && manifest.parsed_version()
                        > semver::Version::parse(&row.version).unwrap_or(semver::Version::new(0, 0, 0))
                {
                    let mut updated = row.clone();
                    updated.version = manifest.version.clone();
                    updated.manifest = domain_manifest_of(&manifest);
                    self.persistence
                        .put_extension(&updated)
                        .map_err(|e| e.to_string())?;
                    let old_dir = self.package_dir(manifest_id, &row.version);
                    if old_dir.exists() {
                        let _ = std::fs::remove_dir_all(&old_dir);
                    }
                }
            }
            None => {
                let granted = permissions_by_name(&manifest.permissions)
                    .map_err(|e| e.to_string())?;
                let row = ExtensionRegistration {
                    id: ExtensionId::new(),
                    manifest: domain_manifest_of(&manifest),
                    version: manifest.version.clone(),
                    installed_at: chrono::Utc::now(),
                    enabled: true,
                    granted_permissions: granted,
                    signature: None, // bundled: trusted by construction
                };
                self.persistence
                    .put_extension(&row)
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
}

impl LoadedAddon {
    /// The §10.12 summary shape.
    pub fn summary(&self) -> AddonSummary {
        let granted: Vec<String> = {
            let mut names: Vec<String> = PermissionGrant::ALL
                .iter()
                .filter(|g| self.registration.granted_permissions.contains(**g))
                .map(|g| g.as_str().to_owned())
                .collect();
            names.sort();
            names
        };
        AddonSummary {
            id: self.manifest.id.clone(),
            version: self.manifest.version.clone(),
            name: self.manifest.name.clone(),
            description: self.manifest.description.clone(),
            author: self.manifest.author.clone(),
            license: self.manifest.license.clone(),
            enabled: self.registration.enabled,
            bundled: self.bundled,
            instance_scope: match self.manifest.instance_scope {
                crate::manifest::InstanceScope::Required => "required".to_owned(),
                crate::manifest::InstanceScope::Optional => "optional".to_owned(),
                crate::manifest::InstanceScope::None => "none".to_owned(),
            },
            permissions: PermissionSummary {
                requested: self.manifest.permissions.clone(),
                granted,
            },
            signature: self.signature_class.as_str(),
            installed_at: self.registration.installed_at,
            page_surfaces: self.manifest.page_surfaces.clone(),
            sidebar_items: self.manifest.sidebar_items.clone(),
            context_menu_items: self.manifest.context_menu_items.clone(),
            commands: self.manifest.commands.clone(),
            event_subscriptions: self.manifest.event_subscriptions.clone(),
            dependencies: self.manifest.dependencies.clone(),
            notice: self.notice.clone(),
        }
    }
}

/// Map permission names onto grants (the closed vocabulary check).
fn permissions_by_name(names: &[String]) -> Result<PermissionSet, ExtensionError> {
    let mut set = PermissionSet::new();
    for name in names {
        let grant = PermissionGrant::ALL
            .iter()
            .find(|g| g.as_str() == name.as_str())
            .copied()
            .ok_or_else(|| ExtensionError::UnknownPermission(name.clone()))?;
        set.grant(grant);
    }
    Ok(set)
}

/// Build the persisted domain manifest from the validated addon
/// manifest: the typed identity core plus the complete addon
/// manifest preserved verbatim under a reserved `extra` key so the
/// serving index can rehydrate exactly what was validated.
fn domain_manifest_of(manifest: &AddonManifest) -> ExtensionManifest {
    let full = serde_json::to_value(manifest).unwrap_or(serde_json::Value::Null);
    ExtensionManifest {
        manifest_version: manifest.manifest_version as u32,
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        description: manifest.description.clone(),
        author: manifest.author.clone(),
        license: manifest.license.clone(),
        api_version: manifest.api_version.clone(),
        engines: manifest
            .engines
            .get("zim")
            .cloned()
            .unwrap_or_else(|| ">=0.0.0".to_owned()),
        permissions: manifest.permissions.clone(),
        instance_scope: match manifest.instance_scope {
            crate::manifest::InstanceScope::Required => DomainInstanceScope::Instance,
            crate::manifest::InstanceScope::Optional => DomainInstanceScope::Optional,
            crate::manifest::InstanceScope::None => DomainInstanceScope::Global,
        },
        extra: serde_json::json!({ "zim_addon_manifest_v1": full }),
    }
}

/// Rehydrate the addon manifest from a persisted domain manifest
/// (the reserved `zim_addon_manifest_v1` key).
fn rehydrate_manifest(row: &ExtensionRegistration) -> Option<AddonManifest> {
    let full = row.manifest.extra.get("zim_addon_manifest_v1")?;
    let bytes = serde_json::to_vec(full).ok()?;
    AddonManifest::from_bytes(&bytes).ok()
}

/// A stand-in manifest for a row whose package is unusable: identity
/// only, everything else empty — the addon is inert.
fn placeholder_manifest(manifest_id: &str) -> AddonManifest {
    AddonManifest {
        id: manifest_id.to_owned(),
        manifest_version: crate::manifest::MANIFEST_VERSION,
        version: "0.0.0".to_owned(),
        name: manifest_id.to_owned(),
        description: String::new(),
        author: String::new(),
        license: String::new(),
        api_version: "v1".to_owned(),
        engines: std::collections::BTreeMap::from([("zim".to_owned(), ">=0.0.0".to_owned())]),
        permissions: vec![],
        instance_scope: crate::manifest::InstanceScope::None,
        page_surfaces: vec![],
        entrypoints: BTreeMap::new(),
        sidebar_items: vec![],
        context_menu_items: vec![],
        commands: vec![],
        event_subscriptions: vec![],
        dependencies: vec![],
        signature: None,
    }
}

/// Recursively load a package tree into a path → bytes map,
/// normalized to forward-slash relative paths.
fn load_tree(dir: &Path) -> Option<BTreeMap<String, Vec<u8>>> {
    let mut files = BTreeMap::new();
    let mut stack = vec![(dir.to_owned(), String::new())];
    while let Some((current, prefix)) = stack.pop() {
        let entries = std::fs::read_dir(&current).ok()?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if path.is_dir() {
                stack.push((path, relative));
            } else {
                let bytes = std::fs::read(&path).ok()?;
                files.insert(relative, bytes);
            }
        }
    }
    if files.is_empty() {
        return None;
    }
    Some(files)
}

/// The sha256 hex digest of a file's bytes (the ETag source).
fn digest_of(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// The major component of an API generation string ("v1.0.0" →
/// "1"), the smoke gate's comparison discipline.
fn api_major(version: &str) -> String {
    version
        .trim_start_matches('v')
        .split('.')
        .next()
        .unwrap_or("0")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{CommandSpec, ContextMenuItem, SidebarItem};
    use zim_engine_persistence_sqlite::SqlitePersistence;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zim-ext-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn service() -> (ExtensionsService, PathBuf) {
        let dir = temp_dir();
        let persistence: Arc<dyn Persistence> = {
            let store =
                SqlitePersistence::open(&dir.join("engine.db")).expect("sqlite store");
            store.migrate().expect("sqlite migrations");
            Arc::new(store)
        };
        let frontend = Arc::new(FrontendService::new(
            dir.join("data"),
            semver::Version::new(0, 0, 1),
            "v1",
        ));
        // seed_bundled: false — the lifecycle tests want an empty
        // registry; seeding is covered by its own test below.
        let service = ExtensionsService::with_bundled(
            persistence,
            frontend,
            &dir.join("data"),
            semver::Version::new(0, 0, 1),
            "v1",
            false,
        );
        (service, dir)
    }

    fn zip_package(manifest_json: &str, extra: &[(&str, &str)]) -> Vec<u8> {
        let mut buffer = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer.start_file(MANIFEST_FILE, options).unwrap();
            std::io::Write::write_all(&mut writer, manifest_json.as_bytes()).unwrap();
            for (name, contents) in extra {
                writer.start_file(*name, options).unwrap();
                std::io::Write::write_all(&mut writer, contents.as_bytes()).unwrap();
            }
            writer.finish().unwrap();
        }
        buffer
    }

    fn manifest_json(id: &str, version: &str, surface: Option<&str>) -> String {
        let mut value = serde_json::json!({
            "manifest_version": 1,
            "id": id,
            "version": version,
            "name": "Test Addon",
            "api_version": "v1",
            "engines": { "zim": ">=0.0.1" },
            "permissions": ["websocket.connect"],
            "instance_scope": "optional",
            "event_subscriptions": ["instance.state_change"],
            "dependencies": []
        });
        if let Some(surface) = surface {
            // The routing contract: the surface's last non-parameter
            // segment is the entrypoint key it must route to.
            let key = surface
                .trim_start_matches("zim://")
                .split('/')
                .rev()
                .find(|segment| !segment.starts_with('{'))
                .unwrap_or("index");
            let mut entrypoints = serde_json::Map::new();
            entrypoints.insert(key.to_owned(), serde_json::json!("pages/pane.html"));
            value["page_surfaces"] = serde_json::json!([surface]);
            value["entrypoints"] = serde_json::Value::Object(entrypoints);
        }
        value.to_string()
    }

    #[test]
    fn install_enable_disable_uninstall_round_trip() {
        let (service, _dir) = service();
        let archive = zip_package(
            &manifest_json("org.example.addon.one", "0.1.0", Some("zim://servers/{id}/one")),
            &[("pages/pane.html", "<p>one</p>")],
        );

        // Install.
        let summary = service.install(&archive).expect("install");
        assert_eq!(summary.id, "org.example.addon.one");
        assert!(summary.enabled);
        assert!(!summary.bundled);
        assert_eq!(summary.permissions.granted, vec!["websocket.connect".to_owned()]);

        // Serving: the declared surface resolves, others don't.
        assert!(service.resolve("servers/abc/one").is_some());
        assert!(service.resolve("servers/abc/two").is_none());
        let served = service.resolve("servers/abc/one").unwrap();
        assert_eq!(served.mime, "text/html; charset=utf-8");

        // Disable: surface stops resolving, list says so.
        service.disable("org.example.addon.one").expect("disable");
        assert!(service.resolve("servers/abc/one").is_none());
        assert!(!service.list()[0].enabled);

        // Enable again.
        service.enable("org.example.addon.one").expect("enable");
        assert!(service.resolve("servers/abc/one").is_some());

        // Uninstall: gone.
        service.uninstall("org.example.addon.one").expect("uninstall");
        assert!(service.list().is_empty());
        assert!(service.resolve("servers/abc/one").is_none());
        assert!(matches!(
            service.uninstall("org.example.addon.one"),
            Err(ExtensionError::NotFound(_))
        ));
    }

    #[test]
    fn surface_collisions_are_refused() {
        let (service, _dir) = service();
        // The default frontend owns zim://servers/{id}/console.
        let archive = zip_package(
            &manifest_json("org.example.addon.collide", "0.1.0", Some("zim://servers/{id}/console")),
            &[("pages/pane.html", "<p>x</p>")],
        );
        assert!(matches!(
            service.install(&archive),
            Err(ExtensionError::SurfaceCollision { .. })
        ));
        // Nothing was written (§4.9's no-partial-install discipline).
        assert!(service.list().is_empty());

        // A fresh surface installs, and a second addon colliding with
        // the first is refused.
        let first = zip_package(
            &manifest_json("org.example.addon.a", "0.1.0", Some("zim://servers/{id}/alpha")),
            &[("pages/pane.html", "<p>a</p>")],
        );
        service.install(&first).expect("first installs");
        let second = zip_package(
            &manifest_json("org.example.addon.b", "0.1.0", Some("zim://servers/{id}/alpha")),
            &[("pages/pane.html", "<p>b</p>")],
        );
        assert!(matches!(
            service.install(&second),
            Err(ExtensionError::SurfaceCollision { .. })
        ));

        // A wildcard surface overlapping an installed literal one is
        // refused too (param-vs-literal overlaps).
        let wildcard = zip_package(
            &manifest_json("org.example.addon.c", "0.1.0", Some("zim://servers/{id}/{page}")),
            &[("pages/pane.html", "<p>c</p>")],
        );
        assert!(matches!(
            service.install(&wildcard),
            Err(ExtensionError::SurfaceCollision { .. })
        ));
    }

    #[test]
    fn updates_and_downgrades() {
        let (service, _dir) = service();
        let v1 = zip_package(
            &manifest_json("org.example.addon.upd", "0.1.0", Some("zim://servers/{id}/upd")),
            &[("pages/pane.html", "<p>v1</p>")],
        );
        service.install(&v1).expect("v1 installs");
        assert_eq!(
            service.resolve("servers/x/upd").unwrap().bytes,
            b"<p>v1</p>".to_vec()
        );

        // Same version → AlreadyExists.
        assert!(matches!(
            service.install(&v1),
            Err(ExtensionError::AlreadyExists(..))
        ));

        // Lower version → DowngradeRefused.
        let v0 = zip_package(
            &manifest_json("org.example.addon.upd", "0.0.9", Some("zim://servers/{id}/upd")),
            &[("pages/pane.html", "<p>v0</p>")],
        );
        assert!(matches!(
            service.install(&v0),
            Err(ExtensionError::DowngradeRefused(..))
        ));

        // Higher version → update, new bytes served, old tree gone.
        let v2 = zip_package(
            &manifest_json("org.example.addon.upd", "0.2.0", Some("zim://servers/{id}/upd")),
            &[("pages/pane.html", "<p>v2</p>")],
        );
        let summary = service.install(&v2).expect("v2 updates");
        assert_eq!(summary.version, "0.2.0");
        assert_eq!(
            service.resolve("servers/x/upd").unwrap().bytes,
            b"<p>v2</p>".to_vec()
        );
        assert_eq!(service.list().len(), 1);
    }

    #[test]
    fn dependencies_gate_installs_and_uninstalls() {
        let (service, _dir) = service();
        let mut manifest = serde_json::from_str::<serde_json::Value>(&manifest_json(
            "org.example.addon.dep",
            "0.1.0",
            None,
        ))
        .unwrap();
        manifest["dependencies"] = serde_json::json!(["org.example.addon.missing@>=1.0.0"]);
        let archive = zip_package(&manifest.to_string(), &[]);
        assert!(matches!(
            service.install(&archive),
            Err(ExtensionError::DependencyUnsatisfied(_))
        ));
        assert!(service.list().is_empty(), "no partial install");

        // A dependency on the default frontend package (present) is
        // satisfied.
        let mut manifest = serde_json::from_str::<serde_json::Value>(&manifest_json(
            "org.example.addon.ok",
            "0.1.0",
            None,
        ))
        .unwrap();
        manifest["dependencies"] =
            serde_json::json!(["org.zaminmc.frontend.default@>=1.0.0"]);
        let archive = zip_package(&manifest.to_string(), &[]);
        assert!(service.install(&archive).is_ok());

        // An addon that others depend on cannot be uninstalled out
        // from under them.
        let mut dependent = serde_json::from_str::<serde_json::Value>(&manifest_json(
            "org.example.addon.needy",
            "0.1.0",
            None,
        ))
        .unwrap();
        dependent["dependencies"] = serde_json::json!(["org.example.addon.ok@>=0.1.0"]);
        let archive = zip_package(&dependent.to_string(), &[]);
        service.install(&archive).expect("dependent installs");
        assert!(matches!(
            service.uninstall("org.example.addon.ok"),
            Err(ExtensionError::HasDependents(..))
        ));
    }

    #[test]
    fn corrupt_package_is_disabled_with_notice_not_fatal() {
        let (service, dir) = service();
        let archive = zip_package(
            &manifest_json("org.example.addon.corrupt", "0.1.0", Some("zim://servers/{id}/crpt")),
            &[("pages/pane.html", "<p>ok</p>")],
        );
        service.install(&archive).expect("install");

        // Corrupt the extracted manifest on disk.
        let manifest_path = dir
            .join("data")
            .join("extensions")
            .join("packages")
            .join("org.example.addon.corrupt")
            .join("0.1.0")
            .join(MANIFEST_FILE);
        std::fs::write(&manifest_path, b"{ not json").expect("corrupt");

        // Rebuild (what boot does): the addon is disabled with a
        // notice; the engine and other addons keep working.
        service.rebuild_index().expect("rebuild");
        let summaries = service.list();
        assert_eq!(summaries.len(), 1);
        assert!(!summaries[0].enabled);
        assert!(summaries[0].notice.is_some());
        assert!(service.resolve("servers/x/crpt").is_none());
    }

    #[test]
    fn boot_after_restart_restores_state() {
        let dir = temp_dir();
        {
            let persistence: Arc<dyn Persistence> = {
                let store =
                    SqlitePersistence::open(&dir.join("engine.db")).expect("sqlite store");
                store.migrate().expect("sqlite migrations");
                Arc::new(store)
            };
            let frontend = Arc::new(FrontendService::new(
                dir.join("data"),
                semver::Version::new(0, 0, 1),
                "v1",
            ));
            let service = ExtensionsService::new(
                persistence.clone(),
                frontend,
                &dir.join("data"),
                semver::Version::new(0, 0, 1),
                "v1",
            );
            let archive = zip_package(
                &manifest_json("org.example.addon.persist", "0.1.0", Some("zim://servers/{id}/pers")),
                &[("pages/pane.html", "<p>p</p>")],
            );
            service.install(&archive).expect("install");
            service.disable("org.example.addon.persist").expect("disable");
        }
        // A fresh service over the same data dir (a reboot).
        {
            let persistence: Arc<dyn Persistence> = {
                let store =
                    SqlitePersistence::open(&dir.join("engine.db")).expect("sqlite store");
                store.migrate().expect("sqlite migrations");
                Arc::new(store)
            };
            let frontend = Arc::new(FrontendService::new(
                dir.join("data"),
                semver::Version::new(0, 0, 1),
                "v1",
            ));
            let service = ExtensionsService::new(
                persistence,
                frontend,
                &dir.join("data"),
                semver::Version::new(0, 0, 1),
                "v1",
            );
            let summaries = service.list();
            assert_eq!(summaries.len(), 1);
            assert!(!summaries[0].enabled, "the disable choice survives reboot");
            // Enabling after reboot works and serves.
            service.enable("org.example.addon.persist").expect("enable");
            assert!(service.resolve("servers/x/pers").is_some());
        }
    }

    #[test]
    fn api_generation_and_engine_range_are_gated() {
        let (service, _dir) = service();
        let mut manifest = serde_json::from_str::<serde_json::Value>(&manifest_json(
            "org.example.addon.gen",
            "0.1.0",
            None,
        ))
        .unwrap();
        manifest["api_version"] = serde_json::json!("v2");
        let archive = zip_package(&manifest.to_string(), &[]);
        assert!(matches!(
            service.install(&archive),
            Err(ExtensionError::ApiGeneration(..))
        ));

        let mut manifest = serde_json::from_str::<serde_json::Value>(&manifest_json(
            "org.example.addon.gen",
            "0.1.0",
            None,
        ))
        .unwrap();
        manifest["engines"] = serde_json::json!({ "zim": ">=99.0.0" });
        let archive = zip_package(&manifest.to_string(), &[]);
        assert!(matches!(
            service.install(&archive),
            Err(ExtensionError::EngineIncompatible(..))
        ));
    }

    #[test]
    fn grant_extends_permissions() {
        let (service, _dir) = service();
        let archive = zip_package(
            &manifest_json("org.example.addon.grant", "0.1.0", Some("zim://servers/{id}/grnt")),
            &[("pages/pane.html", "<p>g</p>")],
        );
        service.install(&archive).expect("install");
        let summary = service
            .grant(
                "org.example.addon.grant",
                &["file.read".to_owned(), "bogus.permission".to_owned()],
            );
        // Unknown permission names are rejected whole.
        assert!(matches!(
            summary,
            Err(ExtensionError::UnknownPermission(_))
        ));
        // The good-only grant applies.
        let summary = service
            .grant("org.example.addon.grant", &["file.read".to_owned()])
            .expect("grant");
        assert!(summary.permissions.granted.contains(&"file.read".to_owned()));
        assert!(summary.permissions.granted.contains(&"websocket.connect".to_owned()));
    }

    #[test]
    fn trusted_signature_tamper_is_a_hard_reject() {
        let (service, _dir) = service();
        let mut manifest: AddonManifest =
            serde_json::from_str(&manifest_json("org.example.addon.sig", "0.1.0", None)).unwrap();
        manifest.validate().expect("valid");
        crate::signature::dev_sign_manifest(&mut manifest);
        // Re-serialize with the signature, then tamper the name so
        // the signature no longer covers the canonical form.
        let mut value = serde_json::to_value(&manifest).unwrap();
        value["name"] = serde_json::json!("Tampered After Signing");
        let archive = zip_package(&value.to_string(), &[]);
        assert!(matches!(
            service.install(&archive),
            Err(ExtensionError::BadSignature)
        ));
    }

    #[test]
    fn summaries_carry_contributions() {
        let (service, _dir) = service();
        let manifest = serde_json::json!({
            "manifest_version": 1,
            "id": "org.example.addon.rich",
            "version": "0.3.0",
            "name": "Rich Addon",
            "description": "Exercises every contribution field.",
            "author": "Example",
            "license": "Apache-2.0",
            "api_version": "v1",
            "engines": { "zim": ">=0.0.1" },
            "permissions": ["file.read"],
            "instance_scope": "required",
            "page_surfaces": ["zim://servers/{id}/rich"],
            "entrypoints": { "rich": "pages/rich.html" },
            "sidebar_items": [
                { "label": "Rich pane", "icon": "spark", "page": "zim://servers/{id}/rich" }
            ],
            "context_menu_items": [
                { "label": "Open rich", "page": "zim://servers/{id}/rich?x=1" }
            ],
            "commands": [ { "id": "rich-cmd", "label": "Rich", "accelerator": "Ctrl+Shift+R" } ],
            "event_subscriptions": ["job.completed"],
            "dependencies": []
        });
        let archive = zip_package(&manifest.to_string(), &[("pages/rich.html", "<p>rich</p>")]);
        let summary = service.install(&archive).expect("install");
        assert_eq!(summary.instance_scope, "required");
        assert_eq!(summary.sidebar_items.len(), 1);
        assert_eq!(summary.context_menu_items.len(), 1);
        assert_eq!(summary.commands[0].id, "rich-cmd");
        assert_eq!(summary.event_subscriptions, vec!["job.completed".to_owned()]);
    }

    #[test]
    fn bundled_defaults_seed_disable_only_and_survive_reboot() {
        let dir = temp_dir();
        let build = || -> ExtensionsService {
            let persistence: Arc<dyn Persistence> = {
                let store =
                    SqlitePersistence::open(&dir.join("engine.db")).expect("sqlite store");
                store.migrate().expect("sqlite migrations");
                Arc::new(store)
            };
            let frontend = Arc::new(FrontendService::new(
                dir.join("data"),
                semver::Version::new(0, 0, 1),
                "v1",
            ));
            ExtensionsService::new(
                persistence,
                frontend,
                &dir.join("data"),
                semver::Version::new(0, 0, 1),
                "v1",
            )
        };

        // Fresh data dir: every embedded default is installed and
        // enabled, and list() puts the bundled ones first.
        let service = build();
        let summaries = service.list();
        assert!(!summaries.is_empty(), "the bundled set is not empty");
        assert!(summaries.iter().all(|s| s.bundled));
        assert!(summaries.iter().all(|s| s.enabled));

        // Bundled addons are disable-only (§6).
        let first = summaries[0].id.clone();
        assert!(matches!(
            service.uninstall(&first),
            Err(ExtensionError::BundledNotRemovable(_))
        ));
        service.disable(&first).expect("bundled can disable");
        assert!(!service.list().iter().find(|s| s.id == first).unwrap().enabled);

        // A reboot keeps the registration and the disable choice,
        // and repairs a missing extracted tree.
        let id = first.clone();
        let version = summaries[0].version.clone();
        let tree = dir
            .join("data")
            .join("extensions")
            .join("packages")
            .join(&id)
            .join(&version);
        assert!(tree.exists());
        let _ = std::fs::remove_dir_all(&tree);
        let rebooted = build();
        let still = rebooted.list().iter().find(|s| s.id == id).unwrap().clone();
        assert!(!still.enabled, "the disable choice survives the reboot");
        assert!(tree.exists(), "the missing tree was re-extracted from the embedded bytes");
    }
}
