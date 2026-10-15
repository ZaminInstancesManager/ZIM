//! The frontend-package manifest (`zim.frontend.json`).
//!
//! Schema and semantics per `docs/api/FRONTEND_EXTENSION_SPEC.md`
//! §3.1. Validation covers: the required identity fields, the
//! manifest/api/engine versions, page-surface well-formedness, the
//! entrypoint map, the declared permission vocabulary, and the
//! (optional) signature block. Path-valued fields must be relative
//! and confinement-safe — a package may never reference files
//! outside itself.

use std::collections::BTreeMap;

use base64::Engine;
use serde::{Deserialize, Serialize};

/// The manifest's file name at a package's root.
pub const MANIFEST_FILE: &str = "zim.frontend.json";

/// The manifest schema major this engine speaks.
pub const MANIFEST_VERSION: u8 = 1;

/// A parsed and structurally validated `zim.frontend.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrontendManifest {
    /// The package's reverse-DNS identifier (e.g.
    /// `org.zaminmc.frontend.default`).
    pub id: String,
    /// Schema major (must be `1`).
    pub manifest_version: u8,
    /// The package's own semver version.
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
    /// The engine API generation this package speaks (`v1`).
    pub api_version: String,
    /// Engine-version compatibility range (`{"zim": ">=0.0.1"}`).
    pub engines: BTreeMap<String, String>,
    /// Whether the package claims the full `zim://` surface set.
    #[serde(default)]
    pub full_replacement: bool,
    /// The configured smoke-test page (§3.5 step 5).
    #[serde(default)]
    pub smoke_test: Option<String>,
    /// The `zim://` URL inventory this package claims (§3.2).
    #[serde(default)]
    pub page_surfaces: Vec<String>,
    /// Surface name → package-root-relative HTML file.
    #[serde(default)]
    pub entrypoints: BTreeMap<String, String>,
    /// Declared asset paths (validated for existence, not type).
    #[serde(default)]
    pub assets: Vec<String>,
    /// Requested permission grants (§3.3).
    #[serde(default)]
    pub permissions: Vec<String>,
    /// The accessibility conformance declaration (§3.7).
    #[serde(default)]
    pub accessibility: Option<AccessibilityDeclaration>,
    /// The author's signature over the canonical manifest (§3.4).
    #[serde(default)]
    pub signature: Option<SignatureInfo>,
}

/// The §3.7 accessibility declaration (a contract, not a check).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessibilityDeclaration {
    /// The claimed standard (e.g. `WCAG 2.1`).
    pub standard: String,
    /// The claimed level (`A`, `AA`, `AAA`).
    pub level: String,
}

/// The §3.4 signature block: an ed25519 signature by the author's
/// key over the canonical manifest JSON (this field removed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignatureInfo {
    /// Signature algorithm; only `ed25519` is understood.
    pub algorithm: String,
    /// The author's ed25519 verifying key, base64 (32 raw bytes).
    pub public_key: String,
    /// The signature, base64 (64 raw bytes).
    pub value: String,
}

/// Every way a manifest can fail validation.
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
pub enum ManifestError {
    /// The manifest was not valid UTF-8.
    #[error("the manifest is not valid UTF-8")]
    NotUtf8,
    /// The manifest was not valid JSON.
    #[error("the manifest is not valid JSON: {0}")]
    NotJson(String),
    /// A required field was missing or empty.
    #[error("field {field:?}: {message}")]
    Missing {
        /// The offending field.
        field: &'static str,
        /// What is wrong with it.
        message: String,
    },
    /// A path-valued field broke package confinement.
    #[error("path in {field:?} escapes the package: {value:?}")]
    PathEscape {
        /// The offending field.
        field: &'static str,
        /// The offending value.
        value: String,
    },
}

impl FrontendManifest {
    /// Parse and validate manifest bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ManifestError> {
        let text = std::str::from_utf8(bytes).map_err(|_| ManifestError::NotUtf8)?;
        let manifest: FrontendManifest =
            serde_json::from_str(text).map_err(|e| ManifestError::NotJson(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Structural validation (the §3.1 schema, as the engine
    /// enforces it).
    pub fn validate(&self) -> Result<(), ManifestError> {
        let missing = |field: &'static str, message: &str| ManifestError::Missing {
            field,
            message: message.to_owned(),
        };

        if self.id.trim().is_empty() {
            return Err(missing("id", "must be a non-empty reverse-DNS identifier"));
        }
        // Reverse-DNS shape: at least one dot, ASCII letters/digits/
        // dots/hyphens only. (The default is org.zaminmc.frontend.default.)
        if !self
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
            || !self.id.contains('.')
        {
            return Err(missing(
                "id",
                "must be ASCII letters, digits, dots and hyphens, with at least one dot",
            ));
        }
        if self.manifest_version != MANIFEST_VERSION {
            return Err(missing(
                "manifest_version",
                &format!("this engine speaks manifest version {MANIFEST_VERSION}"),
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
                "must be a valid semver range (e.g. \">=0.0.1\")",
            ));
        }
        if self.entrypoints.is_empty() {
            return Err(missing(
                "entrypoints",
                "at least one page entrypoint is required",
            ));
        }

        // Path-valued fields: relative, forward-slash, confined.
        for (surface, path) in &self.entrypoints {
            if !safe_package_path(path) {
                return Err(ManifestError::PathEscape {
                    field: "entrypoints",
                    value: format!("{surface} → {path}"),
                });
            }
        }
        if let Some(smoke) = &self.smoke_test {
            if !safe_package_path(smoke) {
                return Err(ManifestError::PathEscape {
                    field: "smoke_test",
                    value: smoke.clone(),
                });
            }
        }
        for asset in &self.assets {
            if !safe_package_path(asset) {
                return Err(ManifestError::PathEscape {
                    field: "assets",
                    value: asset.clone(),
                });
            }
        }

        // Surfaces: well-formed zim:// URLs, no duplicates, and each
        // maps onto an entrypoint key (§3.2 routing contract).
        let mut seen = std::collections::BTreeSet::new();
        for surface in &self.page_surfaces {
            let pattern =
                SurfacePattern::parse(surface).map_err(|message| ManifestError::Missing {
                    field: "page_surfaces",
                    message: format!("{surface:?}: {message}"),
                })?;
            if !seen.insert(surface.clone()) {
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

        // Signature block shape.
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
    /// with the `signature` field removed, keys sorted (serde_json's
    /// default map ordering).
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
}

/// A package-root-relative path is confinement-safe: relative,
/// forward-slash separated, no `..` or empty segments, no drive
/// letters, no backslashes.
pub fn safe_package_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') {
        return false;
    }
    if path.len() > 2 && path.as_bytes()[1] == b':' {
        return false; // Windows drive letter.
    }
    path.split('/')
        .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// One `zim://` surface pattern from a manifest's `page_surfaces`
/// (e.g. `zim://servers/{id}/console`).
///
/// Patterns match concrete URLs segment-by-segment; a `{param}`
/// segment matches exactly one concrete segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfacePattern {
    segments: Vec<String>,
}

impl SurfacePattern {
    /// Parse a declared surface URL.
    pub fn parse(surface: &str) -> Result<Self, String> {
        let rest = surface
            .strip_prefix("zim://")
            .ok_or_else(|| "must start with zim://".to_owned())?;
        let rest = rest.trim_end_matches('/');
        if rest.is_empty() {
            return Err("must name a host path (zim://newtab, not zim://)".to_owned());
        }
        let mut segments = Vec::new();
        for segment in rest.split('/') {
            if segment.is_empty() {
                return Err("contains an empty path segment".to_owned());
            }
            if segment.starts_with('{') || segment.ends_with('}') {
                if segment.len() < 3
                    || !segment.starts_with('{')
                    || !segment.ends_with('}')
                    || !segment[1..segment.len() - 1]
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    return Err(format!("malformed parameter segment {segment:?}"));
                }
            } else if !segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err(format!("invalid characters in segment {segment:?}"));
            }
            segments.push(segment.to_owned());
        }
        Ok(Self { segments })
    }

    /// The entrypoint key this surface routes to: its last
    /// non-parameter segment (`servers/{id}/console` → `console`).
    pub fn entrypoint_key(&self) -> String {
        self.segments
            .iter()
            .rev()
            .find(|s| !s.starts_with('{'))
            .cloned()
            .unwrap_or_else(|| "index".to_owned())
    }

    /// Does a concrete HTTP path (the `zim://` URL minus scheme and
    /// host, e.g. `servers/abc/console`) match this pattern?
    pub fn matches_path(&self, path: &str) -> Option<Vec<(String, String)>> {
        let path = path.trim_matches('/');
        if path.is_empty() {
            return None;
        }
        let segments: Vec<&str> = path.split('/').collect();
        if segments.len() != self.segments.len() {
            return None;
        }
        let mut params = Vec::new();
        for (pattern, concrete) in self.segments.iter().zip(segments) {
            if let Some(name) = pattern.strip_prefix('{').and_then(|p| p.strip_suffix('}')) {
                params.push((name.to_owned(), concrete.to_owned()));
            } else if pattern != concrete {
                return None;
            }
        }
        Some(params)
    }

    /// Could this pattern and another match the same concrete path?
    ///
    /// Two patterns overlap when they have the same segment count
    /// and every segment pair is compatible: equal literals, or at
    /// least one parameter (a `{param}` matches anything, so it
    /// overlaps every literal and every other parameter). M24's
    /// addon loader uses this to refuse page-surface collisions
    /// (FRONTEND_EXTENSION_SPEC.md §4.4): an addon surface may never
    /// overlap the active frontend's or another addon's.
    pub fn overlaps(&self, other: &SurfacePattern) -> bool {
        if self.segments.len() != other.segments.len() {
            return false;
        }
        self.segments
            .iter()
            .zip(&other.segments)
            .all(|(a, b)| a.starts_with('{') || b.starts_with('{') || a == b)
    }

    /// The pattern's concrete prefix, for display.
    pub fn as_str(&self) -> String {
        format!("zim://{}", self.segments.join("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_manifest() -> FrontendManifest {
        FrontendManifest {
            id: "org.example.frontend.minimal".to_owned(),
            manifest_version: 1,
            version: "0.1.0".to_owned(),
            name: "Minimal".to_owned(),
            description: String::new(),
            author: "Example".to_owned(),
            license: "MIT".to_owned(),
            api_version: "v1".to_owned(),
            engines: BTreeMap::from([("zim".to_owned(), ">=0.0.1".to_owned())]),
            full_replacement: false,
            smoke_test: Some("pages/smoke.html".to_owned()),
            page_surfaces: vec![
                "zim://newtab".to_owned(),
                "zim://servers".to_owned(),
                "zim://servers/{id}/console".to_owned(),
            ],
            entrypoints: BTreeMap::from([
                ("newtab".to_owned(), "pages/newtab.html".to_owned()),
                ("servers".to_owned(), "pages/servers.html".to_owned()),
                ("console".to_owned(), "pages/console.html".to_owned()),
            ]),
            assets: vec!["assets/main.css".to_owned()],
            permissions: vec!["websocket.connect".to_owned()],
            accessibility: Some(AccessibilityDeclaration {
                standard: "WCAG 2.1".to_owned(),
                level: "AA".to_owned(),
            }),
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
        let parsed = FrontendManifest::from_bytes(json.as_bytes()).unwrap();
        assert_eq!(parsed.id, "org.example.frontend.minimal");
        assert_eq!(parsed.smoke_test.as_deref(), Some("pages/smoke.html"));
    }

    #[test]
    fn rejects_bad_identity_fields() {
        let mut manifest = valid_manifest();
        manifest.id = "no-dots".to_owned();
        assert!(manifest.validate().is_err());

        manifest.id = "org.example/a b".to_owned();
        assert!(manifest.validate().is_err());

        let mut manifest = valid_manifest();
        manifest.manifest_version = 2;
        assert!(manifest.validate().is_err());

        let mut manifest = valid_manifest();
        manifest.version = "not-semver".to_owned();
        assert!(manifest.validate().is_err());

        let mut manifest = valid_manifest();
        manifest.api_version = String::new();
        assert!(manifest.validate().is_err());

        let mut manifest = valid_manifest();
        manifest.engines.remove("zim");
        assert!(manifest.validate().is_err());

        let mut manifest = valid_manifest();
        manifest
            .engines
            .insert("zim".to_owned(), "garbage range".to_owned());
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn rejects_path_escapes() {
        let mut manifest = valid_manifest();
        manifest
            .entrypoints
            .insert("newtab".to_owned(), "../escape.html".to_owned());
        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::PathEscape {
                field: "entrypoints",
                ..
            })
        ));

        let mut manifest = valid_manifest();
        manifest.assets.push("/absolute.css".to_owned());
        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::PathEscape {
                field: "assets",
                ..
            })
        ));

        let mut manifest = valid_manifest();
        manifest.smoke_test = Some("pages/../../etc/passwd".to_owned());
        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::PathEscape {
                field: "smoke_test",
                ..
            })
        ));

        let mut manifest = valid_manifest();
        manifest
            .entrypoints
            .insert("newtab".to_owned(), "C:\\escape.html".to_owned());
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn rejects_surfaces_that_route_nowhere() {
        let mut manifest = valid_manifest();
        manifest.page_surfaces.push("zim://settings".to_owned());
        // "settings" has no entrypoint → rejected.
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn rejects_malformed_surface_urls() {
        let mut manifest = valid_manifest();
        manifest
            .page_surfaces
            .push("https://example.com".to_owned());
        assert!(manifest.validate().is_err());

        let mut manifest = valid_manifest();
        manifest.page_surfaces.push("zim://".to_owned());
        assert!(manifest.validate().is_err());

        let mut manifest = valid_manifest();
        manifest
            .page_surfaces
            .push("zim://servers//console".to_owned());
        assert!(manifest.validate().is_err());

        let mut manifest = valid_manifest();
        manifest
            .page_surfaces
            .push("zim://servers/{bad name}/console".to_owned());
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn rejects_malformed_signatures() {
        let mut manifest = valid_manifest();
        manifest.signature = Some(SignatureInfo {
            algorithm: "rsa".to_owned(),
            public_key: String::new(),
            value: String::new(),
        });
        assert!(manifest.validate().is_err());

        use base64::Engine;
        let mut manifest = valid_manifest();
        manifest.signature = Some(SignatureInfo {
            algorithm: "ed25519".to_owned(),
            public_key: base64::engine::general_purpose::STANDARD.encode([0u8; 31]),
            value: base64::engine::general_purpose::STANDARD.encode([0u8; 64]),
        });
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn canonical_json_is_stable_and_signature_free() {
        let manifest = valid_manifest();
        let canonical = manifest.canonical_json();
        assert!(!canonical.contains("signature"));
        assert_eq!(canonical, manifest.canonical_json());
        // Sorted keys: "api_version" precedes "assets".
        assert!(canonical.find("\"api_version\"").unwrap() < canonical.find("\"assets\"").unwrap());
    }

    #[test]
    fn surface_patterns_match_concrete_paths() {
        let pattern = SurfacePattern::parse("zim://servers/{id}/console").unwrap();
        assert_eq!(pattern.entrypoint_key(), "console");
        let params = pattern.matches_path("servers/abc-123/console").unwrap();
        assert_eq!(params, vec![("id".to_owned(), "abc-123".to_owned())]);
        assert!(pattern.matches_path("servers/abc/console/extra").is_none());
        assert!(pattern.matches_path("newtab").is_none());

        let fixed = SurfacePattern::parse("zim://newtab").unwrap();
        assert_eq!(fixed.entrypoint_key(), "newtab");
        assert!(fixed.matches_path("newtab").is_some());
        assert!(fixed.matches_path("servers").is_none());
    }

    #[test]
    fn surface_pattern_overlap_semantics() {
        // Same literals overlap; differing literals do not.
        let console = SurfacePattern::parse("zim://servers/{id}/console").unwrap();
        let console_again = SurfacePattern::parse("zim://servers/{id}/console").unwrap();
        let files = SurfacePattern::parse("zim://servers/{id}/files").unwrap();
        assert!(console.overlaps(&console_again));
        assert!(!console.overlaps(&files));

        // A parameter overlaps a literal at the same position.
        let wildcard = SurfacePattern::parse("zim://servers/{id}/{page}").unwrap();
        assert!(console.overlaps(&wildcard));
        assert!(wildcard.overlaps(&console));

        // Different depths never overlap.
        let shallow = SurfacePattern::parse("zim://servers/{id}").unwrap();
        assert!(!console.overlaps(&shallow));

        // Unrelated roots never overlap.
        let newtab = SurfacePattern::parse("zim://newtab").unwrap();
        assert!(!console.overlaps(&newtab));
    }
}
