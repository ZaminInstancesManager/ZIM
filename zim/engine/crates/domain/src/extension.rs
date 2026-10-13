//! Extension registration entities.
//!
//! # Source of truth
//!
//! `ExtensionRegistration` mirrors `docs/architecture/RUST_ENGINE.md`
//! §3. The manifest field set follows the ZIM addon manifest
//! (`zim.addon.json`, version 1) from
//! `docs/api/FRONTEND_EXTENSION_SPEC.md` §4.1. The UI-surface parts
//! of the full manifest (page_surfaces, entrypoints, sidebar_items,
//! commands, event_subscriptions, dependencies) are the frontend
//! loader's concern (M-later); this module models the identity /
//! permission / signature core that the engine persists and the API
//! manages, and keeps the remaining manifest fields as a
//! `#[serde(flatten)]` JSON value so nothing is lost on round-trip.

use crate::id::ExtensionId;
use crate::permission::PermissionSet;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

/// An installed extension (ZIM addon) registration.
///
/// System-level extensions additionally carry a
/// [`ExtensionSignature`] that the loader verifies at install time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionRegistration {
    /// Strongly-typed extension identifier.
    pub id: ExtensionId,
    /// The parsed addon manifest core.
    pub manifest: ExtensionManifest,
    /// The installed package version (recorded at install time; may
    /// trail the manifest's declared version between updates).
    pub version: String,
    /// When the extension was installed.
    pub installed_at: DateTime<Utc>,
    /// Whether the extension is enabled (disabled extensions are
    /// inert but kept installed).
    pub enabled: bool,
    /// The permissions granted at install (admin-approved; cannot be
    /// elevated silently — see FRONTEND_EXTENSION_SPEC.md §4.2).
    pub granted_permissions: PermissionSet,
    /// Required for system-level extensions.
    pub signature: Option<ExtensionSignature>,
}

/// The persisted core of a `zim.addon.json` manifest.
///
/// Identity, engines constraint, permissions, and instance scope.
/// The UI-surface fields are preserved verbatim in `extra` so that
/// round-tripping a registration does not lose manifest data the
/// frontend loader needs later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionManifest {
    /// Manifest format version (currently 1).
    pub manifest_version: u32,
    /// Reverse-DNS extension id (e.g.
    /// `org.example.zim-addon.database-manager`).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Author name.
    pub author: String,
    /// License identifier (e.g. "MIT").
    pub license: String,
    /// The ZIM API version this extension targets (e.g. "v1").
    pub api_version: String,
    /// Engine compatibility constraint, e.g. `">=0.1.0"`.
    pub engines: String,
    /// The permissions requested by the manifest (granted subset is
    /// tracked on the registration).
    pub permissions: Vec<String>,
    /// Whether the extension operates per-instance, globally, or
    /// both.
    pub instance_scope: InstanceScope,
    /// Remaining manifest fields (page surfaces, entrypoints,
    /// sidebar, commands, events, dependencies, ...), preserved
    /// verbatim for the frontend loader.
    #[serde(flatten)]
    pub extra: JsonValue,
}

/// Whether an extension's surfaces are per-instance, global, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InstanceScope {
    /// Extension operates globally (no instance context).
    Global,
    /// Extension operates only within an instance's pages.
    #[default]
    Instance,
    /// Extension has both global and per-instance surfaces.
    Optional,
}

/// A detached signature over the extension package.
///
/// The signature scheme is `ed25519` for ZIM-signed (system-level)
/// extensions; third-party extensions may use `minisign`-style
/// signatures. Verification itself lives in the extension loader,
/// not the domain layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionSignature {
    /// The signature scheme (`ed25519` or `minisign`).
    pub scheme: SignatureScheme,
    /// The signature key identifier (fingerprint of the public key).
    pub key_id: String,
    /// The base64-encoded detached signature.
    pub signature: String,
}

/// Supported signature schemes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SignatureScheme {
    /// Ed25519 detached signature (system-level extensions).
    #[default]
    Ed25519,
    /// minisign-style signature (third-party releases).
    Minisign,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::PermissionGrant;

    fn sample_manifest() -> ExtensionManifest {
        ExtensionManifest {
            manifest_version: 1,
            id: "org.example.zim-addon.database-manager".to_owned(),
            name: "Database Manager".to_owned(),
            description: "Adds a database management UI to ZIM.".to_owned(),
            author: "Example Author".to_owned(),
            license: "MIT".to_owned(),
            api_version: "v1".to_owned(),
            engines: ">=0.1.0".to_owned(),
            permissions: vec![
                "database.read".to_owned(),
                "database.create".to_owned(),
                "database.update".to_owned(),
                "database.delete".to_owned(),
                "database.view_password".to_owned(),
            ],
            instance_scope: InstanceScope::Optional,
            extra: serde_json::json!({
                "page_surfaces": ["zim://servers/{id}/databases"],
                "sidebar_items": [
                    { "label": "Databases", "icon": "database", "page": "zim://servers/{id}/databases" }
                ],
                "event_subscriptions": ["instance.state_change", "job.completed"],
                "dependencies": ["org.zaminmc.frontend.default@>=1.0.0"],
            }),
        }
    }

    fn sample_registration() -> ExtensionRegistration {
        ExtensionRegistration {
            id: ExtensionId::new(),
            manifest: sample_manifest(),
            version: "0.1.0".to_owned(),
            installed_at: Utc::now(),
            enabled: true,
            granted_permissions: {
                let mut set = PermissionSet::new();
                set.grant(PermissionGrant::DatabaseRead)
                    .grant(PermissionGrant::DatabaseCreate)
                    .grant(PermissionGrant::DatabaseUpdate)
                    .grant(PermissionGrant::DatabaseDelete);
                set
            },
            signature: Some(ExtensionSignature {
                scheme: SignatureScheme::Ed25519,
                key_id: "SHA256:7xK3dEfQm2RzX5wY".to_owned(),
                signature: "base64encodedsignature==".to_owned(),
            }),
        }
    }

    #[test]
    fn registration_round_trips_through_serde() {
        let registration = sample_registration();
        let json = serde_json::to_string(&registration).unwrap();
        let back: ExtensionRegistration = serde_json::from_str(&json).unwrap();
        assert_eq!(registration, back);
    }

    #[test]
    fn manifest_preserves_extra_fields_on_round_trip() {
        let manifest = sample_manifest();
        let json = serde_json::to_string(&manifest).unwrap();
        let back: ExtensionManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest, back);

        // The flattened extra fields survived:
        let extra = back.extra.as_object().unwrap();
        assert!(extra.contains_key("page_surfaces"));
        assert!(extra.contains_key("sidebar_items"));
        assert!(extra.contains_key("event_subscriptions"));
        assert!(extra.contains_key("dependencies"));
        assert_eq!(
            extra["event_subscriptions"][1].as_str().unwrap(),
            "job.completed"
        );
    }

    #[test]
    fn manifest_parses_addon_json_shape() {
        // Parse exactly the manifest JSON documented in
        // FRONTEND_EXTENSION_SPEC.md §4.1 (minus $schema, which the
        // loader strips).
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
            "permissions": ["database.read"],
            "instance_scope": "optional",
            "page_surfaces": ["zim://servers/{id}/databases"],
            "commands": [ { "id": "create-database", "label": "Create database" } ]
        }"#;
        let value: JsonValue = serde_json::from_str(raw).unwrap();
        // The engines field arrives as an object in the raw manifest;
        // adapt it to the string form this struct persists.
        let mut adapted = value.clone();
        adapted["engines"] = serde_json::json!(">=0.1.0");
        let manifest: ExtensionManifest = serde_json::from_value(adapted).unwrap();
        assert_eq!(manifest.id, "org.example.zim-addon.database-manager");
        assert_eq!(manifest.instance_scope, InstanceScope::Optional);
        assert!(manifest.extra.get("commands").is_some());
    }

    #[test]
    fn unsigned_registration_round_trips() {
        let mut registration = sample_registration();
        registration.signature = None;
        registration.enabled = false;
        let json = serde_json::to_string(&registration).unwrap();
        let back: ExtensionRegistration = serde_json::from_str(&json).unwrap();
        assert_eq!(registration, back);
        assert!(back.signature.is_none());
        assert!(!back.enabled);
    }

    #[test]
    fn signature_schemes_serialize_lowercase() {
        assert_eq!(
            serde_json::to_string(&SignatureScheme::Ed25519).unwrap(),
            "\"ed25519\""
        );
        assert_eq!(
            serde_json::to_string(&SignatureScheme::Minisign).unwrap(),
            "\"minisign\""
        );
    }

    #[test]
    fn instance_scope_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&InstanceScope::Global).unwrap(),
            "\"global\""
        );
        assert_eq!(
            serde_json::to_string(&InstanceScope::Optional).unwrap(),
            "\"optional\""
        );
        // Round-trip back:
        let scope: InstanceScope = serde_json::from_str("\"optional\"").unwrap();
        assert_eq!(scope, InstanceScope::Optional);
    }
}
