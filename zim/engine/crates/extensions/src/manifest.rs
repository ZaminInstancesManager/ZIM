//! The ZIM addon manifest (`zim.addon.json`).
//!
//! Schema and semantics per `docs/api/FRONTEND_EXTENSION_SPEC.md`
//! §4.1. ZIM addons are a separate ecosystem from Chromium MV3
//! extensions (AD-7): they declare ZIM-specific permissions from the
//! engine's closed vocabulary (`PermissionGrant::as_str`), contribute
//! `zim://` page surfaces, sidebar items, context-menu items,
//! commands, and event subscriptions, and are checked at the engine
//! boundary on install.
//!
//! Validation covers: required identity fields, the manifest/api/
//! engine versions, the closed permission vocabulary, the
//! instance-scope enum, page-surface well-formedness and
//! entrypoint routing, the contribution lists' cross-references
//! (a sidebar item's `page` must be a declared surface; a command's
//! accelerator must not be a reserved browser shortcut, §4.12), the
//! closed event vocabulary, dependency specifiers, and the optional
//! signature block. Path-valued fields must be relative and
//! confinement-safe — an addon may never reference files outside
//! itself.

use std::collections::BTreeMap;

use base64::Engine;
use serde::{Deserialize, Serialize};
use zim_engine_frontend_loader::manifest::{safe_package_path, SignatureInfo, SurfacePattern};

/// The manifest's file name at an addon package's root.
pub const MANIFEST_FILE: &str = "zim.addon.json";

/// The manifest schema major this engine speaks.
pub const MANIFEST_VERSION: u8 = 1;

/// The event names the engine publishes that addons may subscribe to
/// (the streaming hub's closed vocabulary; see `zim_engine_streaming`).
/// An addon declaring anything else fails validation — subscriptions
/// are a capability declaration, not a free-form string list.
pub const KNOWN_EVENTS: &[&str] = &[
    "instance.state_change",
    "instance.resource_usage",
    "instance.console_output",
    "instance.install_progress",
    "job.completed",
    "job.progress",
];

/// Keyboard shortcuts no addon may claim (§4.12: native browser
/// surfaces are not replaceable; the shell reserves the Chrome tab/
/// window lifecycle combos). Normalized lowercase without spaces.
const RESERVED_ACCELERATORS: &[&str] = &[
    "ctrl+t",
    "ctrl+n",
    "ctrl+w",
    "ctrl+shift+t",
    "ctrl+shift+n",
    "ctrl+shift+w",
];

/// A parsed and structurally validated `zim.addon.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddonManifest {
    /// The addon's reverse-DNS identifier (e.g.
    /// `org.example.zim-addon.database-manager`).
    pub id: String,
    /// Schema major (must be `1`).
    pub manifest_version: u8,
    /// The addon's own semver version.
    pub version: String,
    /// Human name.
    pub name: String,
    /// Human description.
    #[serde(default)]
    pub description: String,
    /// Author identity (the signing principal).
    #[serde(default)]
    pub author: String,
    /// SPDX license identifier.
    #[serde(default)]
    pub license: String,
    /// The engine API generation this addon speaks (`v1`).
    pub api_version: String,
    /// Engine-version compatibility range (`{"zim": ">=0.1.0"}`).
    pub engines: BTreeMap<String, String>,
    /// Requested permission grants, from the closed vocabulary
    /// (§4.2; granted at install by admin approval).
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Whether the addon's UI is per-instance, per-instance-
    /// optional, or global (§4.3).
    #[serde(default)]
    pub instance_scope: InstanceScope,
    /// The `zim://` URL inventory this addon contributes (§4.4). An
    /// addon surface may not collide with the default frontend's or
    /// another addon's — the loader enforces that at install.
    #[serde(default)]
    pub page_surfaces: Vec<String>,
    /// Surface name → addon-root-relative HTML file.
    #[serde(default)]
    pub entrypoints: BTreeMap<String, String>,
    /// Sidebar contributions (§4.5).
    #[serde(default)]
    pub sidebar_items: Vec<SidebarItem>,
    /// Context-menu contributions (§4.6).
    #[serde(default)]
    pub context_menu_items: Vec<ContextMenuItem>,
    /// Commands with optional keyboard accelerators (§4.7).
    #[serde(default)]
    pub commands: Vec<CommandSpec>,
    /// Engine events this addon subscribes to (§4.8), from
    /// [`KNOWN_EVENTS`].
    #[serde(default)]
    pub event_subscriptions: Vec<String>,
    /// Dependency specifiers `id@range` (§4.9), resolved at install.
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// The author's signature over the canonical manifest (§3.4
    /// shape, shared with frontend packages).
    #[serde(default)]
    pub signature: Option<SignatureInfo>,
}

/// The §4.3 instance-scope declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum InstanceScope {
    /// The addon's UI is per-instance: always shown under a specific
    /// server (`"required"`).
    #[serde(rename = "required")]
    Required,
    /// Shown only for instances the addon is enabled for
    /// (`"optional"`).
    #[serde(rename = "optional")]
    #[default]
    Optional,
    /// Global: not tied to an instance (`"none"`).
    #[serde(rename = "none")]
    None,
}

/// A §4.5 sidebar contribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidebarItem {
    /// Display label.
    pub label: String,
    /// Icon identifier from the frontend's documented icon set.
    pub icon: String,
    /// Target `zim://` page — must be one of this addon's declared
    /// page surfaces.
    pub page: String,
}

/// A §4.6 context-menu contribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextMenuItem {
    /// Display label.
    pub label: String,
    /// Target `zim://` page (with optional query string).
    pub page: String,
}

/// A §4.7 command registration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSpec {
    /// Command identifier (unique within the addon; slug shape).
    pub id: String,
    /// Display label.
    pub label: String,
    /// Optional keyboard accelerator (e.g. `Ctrl+Shift+D`).
    #[serde(default)]
    pub accelerator: Option<String>,
}

/// Every way an addon manifest can fail validation.
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
pub enum ManifestError {
    /// The manifest was not valid UTF-8.
    #[error("the manifest is not valid UTF-8")]
    NotUtf8,
    /// The manifest was not valid JSON.
    #[error("the manifest is not valid JSON: {0}")]
    NotJson(String),
    /// A required field was missing or invalid.
    #[error("field {field:?}: {message}")]
    Missing {
        /// The offending field.
        field: &'static str,
        /// What is wrong with it.
        message: String,
    },
    /// A path-valued field broke package confinement.
    #[error("path in {field:?} escapes the addon: {value:?}")]
    PathEscape {
        /// The offending field.
        field: &'static str,
        /// The offending value.
        value: String,
    },
}

impl AddonManifest {
    /// Parse and validate manifest bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ManifestError> {
        let text = std::str::from_utf8(bytes).map_err(|_| ManifestError::NotUtf8)?;
        let manifest: AddonManifest =
            serde_json::from_str(text).map_err(|e| ManifestError::NotJson(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Structural validation (the §4.1 schema as the engine enforces
    /// it). Returns `Ok(())` or the first failure.
    pub fn validate(&self) -> Result<(), ManifestError> {
        let missing = |field: &'static str, message: &str| ManifestError::Missing {
            field,
            message: message.to_owned(),
        };

        // Identity: reverse-DNS, same discipline as frontend ids.
        if self.id.trim().is_empty()
            || !self
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
            || !self.id.contains('.')
        {
            return Err(missing(
                "id",
                "must be a non-empty reverse-DNS identifier (ASCII letters, digits, dots, \
                 hyphens; at least one dot)",
            ));
        }
        if self.manifest_version != MANIFEST_VERSION {
            return Err(missing(
                "manifest_version",
                &format!("this engine speaks addon manifest version {MANIFEST_VERSION}"),
            ));
        }
        if semver::Version::parse(&self.version).is_err() {
            return Err(missing("version", "must be a valid semver version"));
        }
        if self.name.trim().is_empty() {
            return Err(missing("name", "must be non-empty"));
        }
        if self.api_version.trim().is_empty() {
            return Err(missing(
                "api_version",
                "must name the API generation (e.g. \"v1\")",
            ));
        }
        let zim_range = self
            .engines
            .get("zim")
            .ok_or_else(|| missing("engines.zim", "must declare the engine compatibility range"))?;
        if semver::VersionReq::parse(zim_range).is_err() {
            return Err(missing(
                "engines.zim",
                "must be a valid semver range (e.g. \">=0.1.0\")",
            ));
        }

        // Permissions: closed vocabulary, no duplicates. The grant
        // names are the canonical Pterodactyl-preserved strings
        // (PermissionGrant::as_str).
        let vocabulary: Vec<&str> = zim_engine_domain::PermissionGrant::ALL
            .iter()
            .map(|g| g.as_str())
            .collect();
        let mut seen = std::collections::BTreeSet::new();
        for permission in &self.permissions {
            if !vocabulary.contains(&permission.as_str()) {
                return Err(missing(
                    "permissions",
                    &format!(
                        "{permission:?} is not in the engine's permission vocabulary"
                    ),
                ));
            }
            if !seen.insert(permission.clone()) {
                return Err(missing(
                    "permissions",
                    &format!("{permission:?} is declared twice"),
                ));
            }
        }

        // Entrypoints: at least one when surfaces are declared;
        // every path confined.
        for (surface, path) in &self.entrypoints {
            if !safe_package_path(path) {
                return Err(ManifestError::PathEscape {
                    field: "entrypoints",
                    value: format!("{surface} → {path}"),
                });
            }
        }

        // Surfaces: well-formed, unique, routed to declared
        // entrypoints (same routing contract as frontend packages).
        let mut surfaces = std::collections::BTreeSet::new();
        for surface in &self.page_surfaces {
            let pattern =
                SurfacePattern::parse(surface).map_err(|message| ManifestError::Missing {
                    field: "page_surfaces",
                    message: format!("{surface:?}: {message}"),
                })?;
            if !surfaces.insert(surface.clone()) {
                return Err(missing(
                    "page_surfaces",
                    &format!("{surface:?} is declared twice"),
                ));
            }
            let key = pattern.entrypoint_key();
            if !self.entrypoints.contains_key(&key) {
                return Err(missing(
                    "page_surfaces",
                    &format!(
                        "{surface:?} routes to entrypoint {key:?}, which the manifest does not declare"
                    ),
                ));
            }
        }
        if !self.page_surfaces.is_empty() && self.entrypoints.is_empty() {
            return Err(missing("entrypoints", "page surfaces require entrypoints"));
        }

        // Sidebar items: label + icon + a page that must be one of
        // the declared surfaces (§4.5 — the item navigates within
        // the addon's own surface set).
        for item in &self.sidebar_items {
            if item.label.trim().is_empty() {
                return Err(missing("sidebar_items", "label must be non-empty"));
            }
            if item.icon.trim().is_empty() || item.icon.len() > 64 {
                return Err(missing("sidebar_items", "icon must be a short identifier"));
            }
            if !surfaces.contains(&item.page) {
                return Err(missing(
                    "sidebar_items",
                    &format!("page {:#?} is not one of this addon's declared surfaces", item.page),
                ));
            }
        }

        // Context-menu items: label + target page. The page may
        // carry a query string (`?action=backup`), so compare the
        // path part against the declared surfaces.
        for item in &self.context_menu_items {
            if item.label.trim().is_empty() {
                return Err(missing("context_menu_items", "label must be non-empty"));
            }
            let path_only = item.page.split('?').next().unwrap_or("");
            if !surfaces.contains(path_only) {
                return Err(missing(
                    "context_menu_items",
                    &format!(
                        "page {:#?} is not one of this addon's declared surfaces",
                        item.page
                    ),
                ));
            }
        }

        // Commands: unique slug ids, non-empty labels, accelerators
        // of a sane shape that never collide with the reserved
        // browser shortcuts (§4.12).
        let mut command_ids = std::collections::BTreeSet::new();
        for command in &self.commands {
            if command.id.trim().is_empty()
                || !command
                    .id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err(missing("commands", "id must be a non-empty slug (letters, digits, -, _)"));
            }
            if !command_ids.insert(command.id.clone()) {
                return Err(missing(
                    "commands",
                    &format!("{:#?} is registered twice", command.id),
                ));
            }
            if command.label.trim().is_empty() {
                return Err(missing("commands", "label must be non-empty"));
            }
            if let Some(accelerator) = &command.accelerator {
                if let Err(message) = validate_accelerator(accelerator) {
                    return Err(missing("commands", &message));
                }
            }
        }

        // Event subscriptions: closed vocabulary, no duplicates.
        let mut events = std::collections::BTreeSet::new();
        for event in &self.event_subscriptions {
            if !KNOWN_EVENTS.contains(&event.as_str()) {
                return Err(missing(
                    "event_subscriptions",
                    &format!(
                        "{event:?} is not an event the engine publishes ({})",
                        KNOWN_EVENTS.join(", ")
                    ),
                ));
            }
            if !events.insert(event.clone()) {
                return Err(missing(
                    "event_subscriptions",
                    &format!("{event:?} is declared twice"),
                ));
            }
        }

        // Dependencies: `id@range`, no self-dependency, no dupes.
        let mut deps = std::collections::BTreeSet::new();
        for dependency in &self.dependencies {
            let (dep_id, range) = parse_dependency(dependency).map_err(|message| {
                ManifestError::Missing {
                    field: "dependencies",
                    message: format!("{dependency:#?}: {message}"),
                }
            })?;
            let _ = range;
            if dep_id == self.id {
                return Err(missing(
                    "dependencies",
                    "an addon cannot depend on itself",
                ));
            }
            if !deps.insert(dependency.clone()) {
                return Err(missing(
                    "dependencies",
                    &format!("{dependency:#?} is declared twice"),
                ));
            }
        }

        // Signature block shape (same as frontend packages).
        if let Some(signature) = &self.signature {
            if signature.algorithm != "ed25519" {
                return Err(missing(
                    "signature.algorithm",
                    "only \"ed25519\" is understood",
                ));
            }
            if base64::engine::general_purpose::STANDARD
                .decode(signature.public_key.trim())
                .map(|k| k.len())
                != Ok(32)
            {
                return Err(missing(
                    "signature.public_key",
                    "must be base64 of 32 raw ed25519 key bytes",
                ));
            }
            if base64::engine::general_purpose::STANDARD
                .decode(signature.value.trim())
                .map(|s| s.len())
                != Ok(64)
            {
                return Err(missing(
                    "signature.value",
                    "must be base64 of 64 raw signature bytes",
                ));
            }
        }

        Ok(())
    }

    /// The canonical JSON this manifest's signature covers (and the
    /// stable form two equal manifests serialize to): the manifest
    /// with the `signature` field removed, keys sorted.
    pub fn canonical_json(&self) -> String {
        let mut value = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        if let Some(map) = value.as_object_mut() {
            map.remove("signature");
        }
        serde_json::to_string(&value).unwrap_or_default()
    }

    /// The manifest's parsed engine-compatibility range.
    pub fn zim_requirement(&self) -> semver::VersionReq {
        self.engines
            .get("zim")
            .and_then(|r| semver::VersionReq::parse(r).ok())
            .unwrap_or_else(|| semver::VersionReq::parse(">=0.0.0").expect("valid fallback"))
    }

    /// The parsed semver version.
    pub fn parsed_version(&self) -> semver::Version {
        semver::Version::parse(&self.version).expect("validate() already checked semver shape")
    }

    /// Whether this addon id is in the bundled default set
    /// (`org.zaminmc.extension.*` — spec §6: installed by default,
    /// disable-only).
    pub fn is_bundled(&self) -> bool {
        self.id.starts_with("org.zaminmc.extension.")
    }
}

/// Split a `id@range` dependency specifier.
///
/// Returns `(id, VersionReq)` or a human-readable failure. The
/// separator is the LAST `@` (ids may not contain `@`; ranges
/// always start with a comparator character or digit). The default
/// frontend package (`org.zaminmc.frontend.default`) is always
/// present — its "version" is the frontend package's own semver,
/// which the loader resolves against the range; the range shape
/// still must parse.
pub fn parse_dependency(spec: &str) -> Result<(&str, semver::VersionReq), String> {
    let Some(at) = spec.rfind('@') else {
        return Err("must be shaped id@range (no '@' found)".to_owned());
    };
    let id = &spec[..at];
    let range = &spec[at + 1..];
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        || !id.contains('.')
    {
        return Err("the id part must be a reverse-DNS identifier".to_owned());
    }
    let range = semver::VersionReq::parse(range).map_err(|e| format!("bad range: {e}"))?;
    Ok((id, range))
}

/// Validate a keyboard accelerator's shape and reserved-ness.
///
/// Accepted shape: `Modifier+Modifier+Key` where modifiers are
/// `Ctrl`/`Cmd`/`Alt`/`Shift` (case-insensitive) and the key is a
/// letter, digit, or F-key. `Cmd` is recorded as-is; the shell
/// translates per-platform. Reserved Chrome shortcuts (§4.12) are
/// rejected: an addon can add shortcuts, never override the
/// tab/window lifecycle ones.
fn validate_accelerator(accelerator: &str) -> Result<(), String> {
    let parts: Vec<&str> = accelerator.split('+').map(str::trim).collect();
    if parts.len() < 2 || parts.len() > 4 {
        return Err(format!(
            "accelerator {accelerator:?} must be Modifier+…+Key (2-4 parts)"
        ));
    }
    let key = parts[parts.len() - 1];
    let valid_key = (key.len() == 1
        && (key.as_bytes()[0].is_ascii_alphabetic() || key.as_bytes()[0].is_ascii_digit()))
        || (key.len() >= 2
            && key.starts_with('F')
            && key[1..].chars().all(|c| c.is_ascii_digit())
            && key[1..].parse::<u8>().map(|n| n > 0 && n <= 12).unwrap_or(false));
    if !valid_key {
        return Err(format!(
            "accelerator key {key:?} must be a letter, digit, or F1-F12"
        ));
    }
    for modifier in &parts[..parts.len() - 1] {
        let normalized = normalize_modifier(modifier);
        if normalized.is_empty() {
            return Err(format!(
                "accelerator modifier {modifier:?} must be Ctrl, Cmd, Alt, or Shift"
            ));
        }
    }
    // Reserved-shortcut check on the normalized form: Ctrl and Cmd
    // both map to the primary modifier.
    let normalized: Vec<String> = accelerator
        .split('+')
        .map(str::trim)
        .map(|p| {
            let m = normalize_modifier(p);
            if m.is_empty() {
                p.to_uppercase()
            } else {
                m
            }
        })
        .collect();
    let joined = normalized.join("+").to_lowercase();
    if RESERVED_ACCELERATORS.contains(&joined.as_str()) {
        return Err(format!(
            "accelerator {accelerator:?} is reserved by the shell (tab/window lifecycle, §4.12)"
        ));
    }
    Ok(())
}

/// Normalize one modifier to its canonical spelling, or "" if not a
/// modifier.
fn normalize_modifier(part: &str) -> String {
    match part.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => "Ctrl".to_owned(),
        "cmd" | "command" | "meta" | "super" => "Ctrl".to_owned(),
        "alt" | "option" => "Alt".to_owned(),
        "shift" => "Shift".to_owned(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_manifest() -> AddonManifest {
        AddonManifest {
            id: "org.example.zim-addon.database-manager".to_owned(),
            manifest_version: 1,
            version: "0.1.0".to_owned(),
            name: "Database Manager".to_owned(),
            description: "Adds a database management UI to ZIM.".to_owned(),
            author: "Example Author".to_owned(),
            license: "MIT".to_owned(),
            api_version: "v1".to_owned(),
            engines: BTreeMap::from([("zim".to_owned(), ">=0.1.0".to_owned())]),
            permissions: vec![
                "database.read".to_owned(),
                "database.view_password".to_owned(),
            ],
            instance_scope: InstanceScope::Optional,
            page_surfaces: vec!["zim://servers/{id}/databases".to_owned()],
            entrypoints: BTreeMap::from([(
                "databases".to_owned(),
                "pages/databases.html".to_owned(),
            )]),
            sidebar_items: vec![SidebarItem {
                label: "Databases".to_owned(),
                icon: "database".to_owned(),
                page: "zim://servers/{id}/databases".to_owned(),
            }],
            context_menu_items: vec![ContextMenuItem {
                label: "Backup database".to_owned(),
                page: "zim://servers/{id}/databases?action=backup".to_owned(),
            }],
            commands: vec![CommandSpec {
                id: "create-database".to_owned(),
                label: "Create database".to_owned(),
                accelerator: Some("Ctrl+Shift+D".to_owned()),
            }],
            event_subscriptions: vec![
                "instance.state_change".to_owned(),
                "job.completed".to_owned(),
            ],
            dependencies: vec!["org.zaminmc.frontend.default@>=1.0.0".to_owned()],
            signature: None,
        }
    }

    #[test]
    fn accepts_a_well_formed_manifest() {
        assert!(valid_manifest().validate().is_ok());
    }

    #[test]
    fn parses_from_bytes() {
        let json = serde_json::to_string(&valid_manifest()).unwrap();
        let parsed = AddonManifest::from_bytes(json.as_bytes()).unwrap();
        assert_eq!(parsed.id, "org.example.zim-addon.database-manager");
        assert_eq!(parsed.instance_scope, InstanceScope::Optional);
    }

    #[test]
    fn parses_the_spec_example() {
        // Exactly the manifest documented in §4.1 (minus $schema).
        let raw = r#"{
            "manifest_version": 1,
            "id": "org.example.zim-addon.database-manager",
            "version": "0.1.0",
            "name": "Database Manager",
            "description": "Adds a database management UI to ZIM.",
            "author": "Example Author",
            "license": "MIT",
            "api_version": "v1",
            "engines": { "zim": ">=0.1.0" },
            "permissions": [
                "database.read", "database.create", "database.update",
                "database.delete", "database.view_password"
            ],
            "instance_scope": "optional",
            "page_surfaces": ["zim://servers/{id}/databases"],
            "entrypoints": { "databases": "pages/databases.html" },
            "sidebar_items": [
                { "label": "Databases", "icon": "database", "page": "zim://servers/{id}/databases" }
            ],
            "context_menu_items": [
                { "label": "Backup database", "page": "zim://servers/{id}/databases?action=backup" }
            ],
            "commands": [
                { "id": "create-database", "label": "Create database", "accelerator": "Ctrl+Shift+D" }
            ],
            "event_subscriptions": ["instance.state_change", "job.completed"],
            "dependencies": ["org.zaminmc.frontend.default@>=1.0.0"]
        }"#;
        let parsed = AddonManifest::from_bytes(raw.as_bytes()).unwrap();
        assert_eq!(parsed.sidebar_items.len(), 1);
        assert_eq!(parsed.commands[0].id, "create-database");
        assert_eq!(parsed.permissions.len(), 5);
    }

    #[test]
    fn rejects_bad_identity_fields() {
        let mut manifest = valid_manifest();
        manifest.id = "no-dots".to_owned();
        assert!(manifest.validate().is_err());

        let mut manifest = valid_manifest();
        manifest.manifest_version = 2;
        assert!(manifest.validate().is_err());

        let mut manifest = valid_manifest();
        manifest.version = "not-semver".to_owned();
        assert!(manifest.validate().is_err());

        let mut manifest = valid_manifest();
        manifest.engines.remove("zim");
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn rejects_permissions_outside_the_vocabulary() {
        let mut manifest = valid_manifest();
        manifest.permissions.push("god.mode".to_owned());
        assert!(manifest.validate().is_err());

        // Duplicates are rejected too.
        let mut manifest = valid_manifest();
        manifest
            .permissions
            .push(manifest.permissions[0].clone());
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn rejects_path_escapes() {
        let mut manifest = valid_manifest();
        manifest
            .entrypoints
            .insert("databases".to_owned(), "../escape.html".to_owned());
        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::PathEscape { field: "entrypoints", .. })
        ));
    }

    #[test]
    fn rejects_unrouted_surfaces_and_sidebar_targets() {
        let mut manifest = valid_manifest();
        manifest.page_surfaces.push("zim://settings".to_owned());
        assert!(manifest.validate().is_err());

        let mut manifest = valid_manifest();
        manifest.sidebar_items[0].page = "zim://servers/{id}/files".to_owned();
        assert!(manifest.validate().is_err());

        // A context-menu target outside the declared surfaces (the
        // query-string form of a declared one is fine).
        let mut manifest = valid_manifest();
        manifest.context_menu_items[0].page = "zim://app".to_owned();
        assert!(manifest.validate().is_err());
        manifest.context_menu_items[0].page =
            "zim://servers/{id}/databases?action=backup".to_owned();
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn rejects_bad_commands_and_reserved_accelerators() {
        let mut manifest = valid_manifest();
        manifest.commands.push(CommandSpec {
            id: "create-database".to_owned(),
            label: "Duplicate".to_owned(),
            accelerator: None,
        });
        assert!(manifest.validate().is_err());

        for reserved in ["Ctrl+T", "Cmd+N", "Ctrl+W", "Ctrl+Shift+T"] {
            let mut manifest = valid_manifest();
            manifest.commands[0].accelerator = Some(reserved.to_owned());
            assert!(manifest.validate().is_err(), "{reserved} must be reserved");
        }

        // Non-reserved accelerators of a sane shape pass.
        for ok in ["Ctrl+Shift+D", "Alt+G", "Cmd+Alt+9", "Ctrl+F5"] {
            let mut manifest = valid_manifest();
            manifest.commands[0].accelerator = Some(ok.to_owned());
            assert!(manifest.validate().is_ok(), "{ok} must be accepted");
        }

        // Nonsense shapes are rejected.
        for bad in ["D", "Ctrl+", "Win+L", "Ctrl+Shift+NotAKey", "Ctrl+Alt+Shift+Meta+X"] {
            let mut manifest = valid_manifest();
            manifest.commands[0].accelerator = Some(bad.to_owned());
            assert!(manifest.validate().is_err(), "{bad} must be rejected");
        }
    }

    #[test]
    fn rejects_unknown_events_and_self_dependencies() {
        let mut manifest = valid_manifest();
        manifest.event_subscriptions.push("made.up.event".to_owned());
        assert!(manifest.validate().is_err());

        let mut manifest = valid_manifest();
        manifest
            .dependencies
            .push("org.example.zim-addon.database-manager@>=1.0.0".to_owned());
        assert!(manifest.validate().is_err());

        let mut manifest = valid_manifest();
        manifest.dependencies.push("not-a-spec".to_owned());
        assert!(manifest.validate().is_err());

        let mut manifest = valid_manifest();
        manifest
            .dependencies
            .push("org.example.other@not-a-range".to_owned());
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn canonical_json_is_stable_and_signature_free() {
        let manifest = valid_manifest();
        let canonical = manifest.canonical_json();
        assert!(!canonical.contains("signature"));
        assert_eq!(canonical, manifest.canonical_json());
        assert!(
            canonical.find("\"api_version\"").unwrap() < canonical.find("\"author\"").unwrap(),
            "keys are sorted"
        );
    }

    #[test]
    fn dependency_parsing_round_trips() {
        let (id, range) = parse_dependency("org.example.addon@>=1.2.0").unwrap();
        assert_eq!(id, "org.example.addon");
        assert!(range.matches(&semver::Version::parse("1.5.0").unwrap()));
        assert!(!range.matches(&semver::Version::parse("1.0.0").unwrap()));

        assert!(parse_dependency("org.example.addon").is_err());
        assert!(parse_dependency("@>=1.0.0").is_err());
        assert!(parse_dependency("org.example.addon@>=1.0.0 extra").is_err());
    }

    #[test]
    fn bundled_detection_matches_the_default_set() {
        let mut manifest = valid_manifest();
        assert!(!manifest.is_bundled());
        manifest.id = "org.zaminmc.extension.audit-log".to_owned();
        assert!(manifest.is_bundled());
    }
}
