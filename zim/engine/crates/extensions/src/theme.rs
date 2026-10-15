//! Packaged themes (M24 delivers M23's deferred upload surface).
//!
//! A packaged theme is a ZIP with a `zim.theme.json` at the root:
//!
//! ```json
//! {
//!   "manifest_version": 1,
//!   "name": "My Theme",
//!   "description": "…",
//!   "tokens": { "color.background": "#101014", "color.accent": "#e07b39" }
//! }
//! ```
//!
//! Validation is against `zim/contracts/theme-tokens.v1.json` — the
//! engine-defined token vocabulary (FRONTEND_EXTENSION_SPEC.md §2):
//! token names must exist in the contract, values are literal
//! strings or numbers (no code, no new tokens), omitted tokens fall
//! back to the contract's defaults at apply time (the frontend's
//! `var(--…, fallback)` discipline). Uploaded themes live in
//! `{data_dir}/themes/{slug}.json` and are listed by
//! `GET /api/v1/themes`; the frontend's theme panel merges them with
//! its compiled-in presets.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

/// The theme package's manifest file name.
pub const THEME_MANIFEST_FILE: &str = "zim.theme.json";

/// The token vocabulary contract, embedded at compile time from the
/// repo's `zim/contracts/` (three sources, one vocabulary: the
/// contract, the frontend's `main.css`/`theme.ts`, and this loader).
const TOKENS_CONTRACT: &str = include_str!("../../../../contracts/theme-tokens.v1.json");

/// Hard caps: the manifest stays small (a name, a description, and
/// at most the 24 contract tokens).
const MAX_THEME_BYTES: u64 = 1024 * 1024;
const MAX_NAME_LEN: usize = 64;
const MAX_DESCRIPTION_LEN: usize = 256;
const MAX_TOKEN_VALUE_LEN: usize = 256;

/// Every way a theme upload can fail.
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
pub enum ThemeError {
    /// The archive is unusable.
    #[error("the upload is not a valid ZIP archive: {0}")]
    BadArchive(String),
    /// The theme manifest failed validation.
    #[error("{0}")]
    BadTheme(String),
    /// A theme with the same slug exists.
    #[error("a theme named {0} is already installed")]
    AlreadyExists(String),
    /// A data-directory failure.
    #[error("theme storage error: {0}")]
    Storage(String),
}

/// The parsed `zim.theme.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeManifest {
    /// Schema major (must be `1`).
    pub manifest_version: u8,
    /// Human theme name.
    pub name: String,
    /// Optional human description.
    #[serde(default)]
    pub description: String,
    /// Token values, keyed by contract token name.
    #[serde(default)]
    pub tokens: BTreeMap<String, serde_json::Value>,
}

/// One stored theme (the list/upload shape; `Deserialize` because the
/// registry reloads the JSON files it wrote).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeSummary {
    /// The slug id (URL-safe, derived from the name).
    pub id: String,
    /// Human name.
    pub name: String,
    /// Human description.
    pub description: String,
    /// Token values as display strings.
    pub tokens: BTreeMap<String, String>,
    /// The token names this theme actually overrides (the rest fall
    /// back to the contract defaults).
    pub overrides: Vec<String>,
}

/// The theme registry: uploaded themes under
/// `{data_dir}/themes/`, listed for the frontend's theme panel.
pub struct ThemeService {
    themes_dir: PathBuf,
    themes: RwLock<BTreeMap<String, ThemeSummary>>,
}

impl ThemeService {
    /// Load the registry from `{data_dir}/themes/` (missing dir =
    /// empty registry; a corrupt file is skipped, never fatal).
    pub fn new(data_dir: &Path) -> Self {
        let themes_dir = data_dir.join("themes");
        let mut themes = BTreeMap::new();
        if let Ok(entries) = std::fs::read_dir(&themes_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(bytes) = std::fs::read(&path) {
                    if let Ok(summary) = serde_json::from_slice::<ThemeSummary>(&bytes) {
                        themes.insert(summary.id.clone(), summary);
                    }
                }
            }
        }
        Self {
            themes_dir,
            themes: RwLock::new(themes),
        }
    }

    /// Every uploaded theme, by id.
    pub fn list(&self) -> Vec<ThemeSummary> {
        self.themes
            .read()
            .expect("themes lock")
            .values()
            .cloned()
            .collect()
    }

    /// `POST /api/v1/themes` — install from ZIP bytes (the M23
    /// contract note's format: `.zip` with `zim.theme.json` at the
    /// root).
    pub fn upload(&self, bytes: &[u8]) -> Result<ThemeSummary, ThemeError> {
        if bytes.len() as u64 > MAX_THEME_BYTES {
            return Err(ThemeError::BadArchive(format!(
                "{} bytes exceeds the {}-byte ceiling",
                bytes.len(),
                MAX_THEME_BYTES
            )));
        }
        let cursor = std::io::Cursor::new(bytes);
        let mut archive =
            zip::ZipArchive::new(cursor).map_err(|e| ThemeError::BadArchive(e.to_string()))?;
        let manifest_bytes = archive.by_name(THEME_MANIFEST_FILE).map_err(|_| {
            ThemeError::BadArchive(format!(
                "the archive has no {THEME_MANIFEST_FILE} at its root"
            ))
        })?;
        let mut manifest_bytes = manifest_bytes;
        let mut raw = Vec::new();
        use std::io::Read;
        manifest_bytes
            .read_to_end(&mut raw)
            .map_err(|e| ThemeError::BadArchive(format!("reading {THEME_MANIFEST_FILE}: {e}")))?;
        let summary = validate_theme(&raw)?;

        let file = self.themes_dir.join(format!("{}.json", summary.id));
        if file.exists() {
            return Err(ThemeError::AlreadyExists(summary.name.clone()));
        }
        std::fs::create_dir_all(&self.themes_dir).map_err(|e| {
            ThemeError::Storage(format!("creating {}: {e}", self.themes_dir.display()))
        })?;
        std::fs::write(&file, serde_json::to_vec(&summary).unwrap_or_default())
            .map_err(|e| ThemeError::Storage(format!("writing {}: {e}", file.display())))?;
        self.themes
            .write()
            .expect("themes lock")
            .insert(summary.id.clone(), summary.clone());
        Ok(summary)
    }

    /// `DELETE /api/v1/themes/{id}` — remove an uploaded theme (the
    /// compiled-in presets are the frontend's, never the engine's to
    /// delete).
    pub fn delete(&self, id: &str) -> Result<(), ThemeError> {
        let slug = slugify(id);
        if slug != id {
            return Err(ThemeError::BadTheme(format!("{id:?} is not a theme id")));
        }
        let removed = self
            .themes
            .write()
            .expect("themes lock")
            .remove(id)
            .ok_or_else(|| ThemeError::BadTheme(format!("no theme {id}")))?;
        let file = self.themes_dir.join(format!("{}.json", removed.id));
        if file.exists() {
            std::fs::remove_file(&file)
                .map_err(|e| ThemeError::Storage(format!("removing {}: {e}", file.display())))?;
        }
        Ok(())
    }
}

/// Validate a `zim.theme.json`'s bytes into a summary.
fn validate_theme(raw: &[u8]) -> Result<ThemeSummary, ThemeError> {
    let manifest: ThemeManifest = serde_json::from_slice(raw)
        .map_err(|e| ThemeError::BadTheme(format!("zim.theme.json is not valid JSON: {e}")))?;
    if manifest.manifest_version != 1 {
        return Err(ThemeError::BadTheme(
            "manifest_version must be 1".to_owned(),
        ));
    }
    if manifest.name.trim().is_empty() || manifest.name.len() > MAX_NAME_LEN {
        return Err(ThemeError::BadTheme(format!(
            "name must be 1-{MAX_NAME_LEN} characters"
        )));
    }
    if manifest.description.len() > MAX_DESCRIPTION_LEN {
        return Err(ThemeError::BadTheme(format!(
            "description must be at most {MAX_DESCRIPTION_LEN} characters"
        )));
    }

    // The closed token vocabulary from the contract.
    let contract: serde_json::Value =
        serde_json::from_str(TOKENS_CONTRACT).expect("the embedded token contract is valid JSON");
    let vocabulary: Vec<&str> = contract
        .get("tokens")
        .and_then(|tokens| tokens.as_object())
        .map(|tokens| tokens.keys().map(String::as_str).collect())
        .unwrap_or_default();

    let mut tokens = BTreeMap::new();
    for (name, value) in &manifest.tokens {
        if !vocabulary.contains(&name.as_str()) {
            return Err(ThemeError::BadTheme(format!(
                "token {name:?} is not in the theme-tokens.v1 vocabulary"
            )));
        }
        let display = match value {
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Number(number) => number.to_string(),
            _ => {
                return Err(ThemeError::BadTheme(format!(
                    "token {name:?} must be a literal string or number (no code, no structures)"
                )))
            }
        };
        if display.len() > MAX_TOKEN_VALUE_LEN {
            return Err(ThemeError::BadTheme(format!(
                "token {name:?} exceeds the {MAX_TOKEN_VALUE_LEN}-character value cap"
            )));
        }
        tokens.insert(name.clone(), display);
    }

    let mut overrides: Vec<String> = tokens.keys().cloned().collect();
    overrides.sort();
    Ok(ThemeSummary {
        id: slugify(&manifest.name),
        name: manifest.name.trim().to_owned(),
        description: manifest.description.trim().to_owned(),
        tokens,
        overrides,
    })
}

/// The URL-safe slug form of a theme name.
fn slugify(name: &str) -> String {
    let slug: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c
            } else if c == ' ' || c == '_' || c == '.' {
                '-'
            } else {
                '-'
            }
        })
        .collect();
    let collapsed = slug
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if collapsed.is_empty() {
        "theme".to_owned()
    } else {
        collapsed.chars().take(64).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme_zip(manifest_json: &str) -> Vec<u8> {
        let mut buffer = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer.start_file(THEME_MANIFEST_FILE, options).unwrap();
            std::io::Write::write_all(&mut writer, manifest_json.as_bytes()).unwrap();
            writer.finish().unwrap();
        }
        buffer
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zim-theme-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn upload_list_delete_round_trip() {
        let dir = temp_dir();
        let service = ThemeService::new(&dir);
        assert!(service.list().is_empty());

        let archive = theme_zip(
            r##"{
                "manifest_version": 1,
                "name": "Ember Dusk",
                "description": "A warm dark theme.",
                "tokens": { "color.background": "#171310", "color.accent": "#e07b39" }
            }"##,
        );
        let summary = service.upload(&archive).expect("upload");
        assert_eq!(summary.id, "ember-dusk");
        assert_eq!(
            summary.overrides,
            vec!["color.accent".to_owned(), "color.background".to_owned()]
        );
        assert_eq!(
            summary.tokens.get("color.accent").map(String::as_str),
            Some("#e07b39")
        );

        assert_eq!(service.list().len(), 1);

        // Same name → conflict.
        assert!(matches!(
            service.upload(&archive),
            Err(ThemeError::AlreadyExists(_))
        ));

        // Reload from disk (a reboot).
        let reloaded = ThemeService::new(&dir);
        assert_eq!(reloaded.list().len(), 1);
        assert_eq!(reloaded.list()[0].id, "ember-dusk");

        // Delete → gone, and a re-upload is allowed.
        service.delete("ember-dusk").expect("delete");
        assert!(service.list().is_empty());
        assert!(service.upload(&archive).is_ok());
    }

    #[test]
    fn rejects_unknown_tokens_and_non_literals() {
        let service = ThemeService::new(&temp_dir());

        let archive = theme_zip(
            r##"{ "manifest_version": 1, "name": "Bad", "tokens": { "color.brand-new": "#fff" } }"##,
        );
        assert!(matches!(
            service.upload(&archive),
            Err(ThemeError::BadTheme(_))
        ));

        let archive = theme_zip(
            r#"{ "manifest_version": 1, "name": "Bad", "tokens": { "color.accent": { "css": "evil()" } } }"#,
        );
        assert!(matches!(
            service.upload(&archive),
            Err(ThemeError::BadTheme(_))
        ));

        let archive = theme_zip(r#"{ "manifest_version": 2, "name": "Bad", "tokens": {} }"#);
        assert!(matches!(
            service.upload(&archive),
            Err(ThemeError::BadTheme(_))
        ));

        let archive = theme_zip(r#"{ "manifest_version": 1, "name": "", "tokens": {} }"#);
        assert!(matches!(
            service.upload(&archive),
            Err(ThemeError::BadTheme(_))
        ));
    }

    #[test]
    fn rejects_archives_without_the_manifest() {
        let service = ThemeService::new(&temp_dir());
        let mut buffer = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
            writer
                .start_file("readme.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut writer, b"not a theme").unwrap();
            writer.finish().unwrap();
        }
        assert!(matches!(
            service.upload(&buffer),
            Err(ThemeError::BadArchive(_))
        ));
    }

    #[test]
    fn slugs_are_stable_and_url_safe() {
        assert_eq!(slugify("Ember Dusk"), "ember-dusk");
        assert_eq!(slugify("  ZIM  Dark  "), "zim-dark");
        assert_eq!(slugify("Motörhead!!"), "mot-rhead");
        assert_eq!(slugify("---"), "theme");
    }
}
