//! The legacy Pterodactyl egg JSON importer.
//!
//! Converts a legacy egg (the community `PTDL_v1`/`PTDL_v2` JSON
//! document) into a ZIM [`Egg`] row per
//! `docs/compatibility/MIGRATION_AND_COMPATIBILITY.md` §3:
//! variables, startup template, install script, and config parser
//! rules are *preserved*; unknown features surface as warnings and
//! are ignored; the security-relevant feature flags
//! (`priviledged_install_scripts`, `requires_root`) are rejected
//! unless the [`ImportPolicy`] explicitly opts in.
//!
//! Leniency contract: a document that is structurally an egg
//! imports unless it violates a domain invariant or a security
//! policy — everything uncertain becomes a warning, never a
//! silent behavior change.

use chrono::Utc;
use serde_json::Value;

use zim_engine_domain::value::DockerImage;
use zim_engine_domain::{
    CommandTemplate, Egg, EggFeatures, EggImage, EggScript, EggScripts, EggVariable, EggVariableId,
    FileParserRule, ParserKind, ReplaceRule, StopConfig,
};

use crate::{EggImportError, ImportPolicy, ImportReport};

/// The startup placeholders every legacy egg may reference without
/// a matching variable declaration (the Wings vocabulary preserved
/// per MIGRATION_AND_COMPATIBILITY.md §3.4).
const INTRINSIC_PLACEHOLDERS: &[&str] = &["SERVER_MEMORY", "SERVER_IP", "SERVER_PORT"];

/// The top-level keys the importer understands (everything else
/// becomes an "unknown field ignored" warning).
const KNOWN_TOP_LEVEL: &[&str] = &[
    "_comment",
    "meta",
    "name",
    "description",
    "author",
    "uuid",
    "features",
    "docker_image",
    "docker_images",
    "file_denylist",
    "startup",
    "config",
    "scripts",
    "variables",
];

/// Import a legacy egg JSON document (see the module docs).
pub fn import_legacy_json(
    source: &str,
    policy: &ImportPolicy,
) -> Result<ImportReport, EggImportError> {
    let document: Value = serde_json::from_str(source)
        .map_err(|e| EggImportError::Json(format!("not valid JSON: {e}")))?;
    let object = document
        .as_object()
        .ok_or_else(|| EggImportError::Json("the egg document must be a JSON object".to_owned()))?;

    let mut warnings = Vec::new();

    // Unknown top-level fields are surfaced, then ignored.
    for key in object.keys() {
        if !KNOWN_TOP_LEVEL.contains(&key.as_str()) {
            warnings.push(format!(
                "unknown field {key:?} ignored (the legacy format is community-evolved)"
            ));
        }
    }

    // ── Identity ────────────────────────────────────────────────
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if name.is_empty() {
        return Err(EggImportError::MissingName);
    }
    let description = object
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let author = object
        .get("author")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let legacy_uuid = object
        .get("uuid")
        .and_then(Value::as_str)
        .map(str::to_owned);

    // ── Meta ────────────────────────────────────────────────────
    if let Some(meta) = object.get("meta").and_then(Value::as_object) {
        let version = meta.get("version").and_then(Value::as_str).unwrap_or("");
        if version != "PTDL_v1" && version != "PTDL_v2" {
            warnings.push(format!(
                "unknown egg meta version {version:?} (expected PTDL_v1 or PTDL_v2); importing anyway"
            ));
        }
        if meta
            .get("update_url")
            .map(Value::is_some_and_not_null)
            .unwrap_or(false)
        {
            warnings.push(
                "meta.update_url ignored (egg auto-updates are not part of the import; re-import \
                 the updated egg)"
                    .to_owned(),
            );
        }
    } else {
        warnings.push("no meta block; assuming the documented PTDL shape".to_owned());
    }

    // ── Features (security-relevant) ────────────────────────────
    let features = parse_features(object.get("features"), &mut warnings)?;
    if features.privileged_install_scripts && !policy.allow_privileged_install_scripts {
        return Err(EggImportError::PrivilegedInstallScripts);
    }
    if features.requires_root && !policy.allow_requires_root {
        return Err(EggImportError::RequiresRoot);
    }

    // ── Docker images ───────────────────────────────────────────
    let mut images = Vec::new();
    if let Some(map) = object.get("docker_images").and_then(Value::as_object) {
        for (label, raw) in map {
            push_image(&mut images, &mut warnings, label, raw);
        }
    } else if let Some(raw) = object.get("docker_image") {
        push_image(&mut images, &mut warnings, "default", raw);
    }
    if images.is_empty() {
        warnings.push(
            "the egg declares no usable docker image; it imports, but instances cannot be \
             created from it until an image is added"
                .to_owned(),
        );
    }

    // ── Startup template ────────────────────────────────────────
    let startup_raw = object
        .get("startup")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if startup_raw.is_empty() {
        return Err(EggImportError::MissingStartup);
    }
    let startup = CommandTemplate::new(startup_raw);

    // ── Scripts ─────────────────────────────────────────────────
    let scripts = parse_scripts(object.get("scripts"), &mut warnings);

    // ── Config parser rules ─────────────────────────────────────
    let config = parse_config(object.get("config"), &mut warnings);

    // ── Variables ───────────────────────────────────────────────
    let mut variables = Vec::new();
    if let Some(list) = object.get("variables").and_then(Value::as_array) {
        for (index, entry) in list.iter().enumerate() {
            match parse_variable(entry) {
                Ok(variable) => variables.push(variable),
                Err(reason) => warnings.push(format!("variables[{index}] skipped: {reason}")),
            }
        }
    }

    // Placeholder references: every {{NAME}} must be an intrinsic
    // or a declared variable — anything else is a warning (the egg
    // may still work with per-instance variables).
    let declared: Vec<&str> = variables.iter().map(|v| v.env_name.as_str()).collect();
    for name in startup.variables() {
        if !INTRINSIC_PLACEHOLDERS.contains(&name.as_str()) && !declared.contains(&name.as_str()) {
            warnings.push(format!(
                "the startup template references {name:?}, which is neither an intrinsic \
                 ({INTRINSIC_PLACEHOLDERS:?}) nor a declared variable"
            ));
        }
    }

    let now = Utc::now();
    let egg = Egg {
        id: zim_engine_domain::EggId::new(),
        legacy_uuid,
        name,
        description,
        author,
        images,
        startup,
        features,
        scripts,
        config,
        variables,
        created_at: now,
        updated_at: now,
    };
    egg.validate().map_err(EggImportError::Domain)?;
    Ok(ImportReport { egg, warnings })
}

/// Whether a JSON value is `Some` and not null (a small readability
/// helper for the meta block).
trait ValueExt {
    fn is_some_and_not_null(&self) -> bool;
}

impl ValueExt for Value {
    fn is_some_and_not_null(&self) -> bool {
        !self.is_null()
    }
}

/// Map the legacy `features` value (string array or object) onto
/// the canonical flags; unknown entries warn.
fn parse_features(
    raw: Option<&Value>,
    warnings: &mut Vec<String>,
) -> Result<EggFeatures, EggImportError> {
    let mut features = EggFeatures::default();
    let Some(raw) = raw else {
        return Ok(features);
    };
    match raw {
        // The eggs-repo shape: an array of feature strings.
        Value::Array(entries) => {
            for entry in entries {
                match entry.as_str() {
                    Some("eula") => features.eula = true,
                    // Pterodactyl's own misspelling, preserved.
                    Some("priviledged_install_scripts") | Some("privileged_install_scripts") => {
                        features.privileged_install_scripts = true;
                    }
                    Some("requires_root") => features.requires_root = true,
                    Some(other) => {
                        warnings.push(format!("unknown feature {other:?} ignored"));
                    }
                    None => warnings.push("non-string feature entry ignored".to_owned()),
                }
            }
        }
        // The canonical ZIM shape: an object.
        Value::Object(map) => {
            for (key, value) in map {
                let flag = value.as_bool().unwrap_or(false);
                match key.as_str() {
                    "eula" => features.eula = flag,
                    "priviledged_install_scripts" | "privileged_install_scripts" => {
                        features.privileged_install_scripts = flag;
                    }
                    "requires_root" => features.requires_root = flag,
                    other => warnings.push(format!("unknown feature {other:?} ignored")),
                }
            }
        }
        _ => warnings.push("features must be an array or an object; ignoring it".to_owned()),
    }
    Ok(features)
}

/// Parse one labeled docker image; malformed references warn and
/// are skipped.
fn push_image(images: &mut Vec<EggImage>, warnings: &mut Vec<String>, label: &str, raw: &Value) {
    let text = raw.as_str().unwrap_or_default().trim();
    if text.is_empty() {
        warnings.push(format!("image {label:?} has an empty reference; skipped"));
        return;
    }
    match text.parse::<DockerImage>() {
        Ok(image) => images.push(EggImage {
            label: label.to_owned(),
            image,
        }),
        Err(e) => warnings.push(format!(
            "image {label:?} ({text:?}) is malformed; skipped: {e}"
        )),
    }
}

/// Map the legacy `scripts` block (`installation`/`install` +
/// `update`); each script runs in its constrained container.
fn parse_scripts(raw: Option<&Value>, warnings: &mut Vec<String>) -> EggScripts {
    let mut scripts = EggScripts::default();
    let Some(object) = raw.and_then(Value::as_object) else {
        return scripts;
    };
    // v2 names it `installation`; v1 named it `install`.
    let install = object.get("installation").or_else(|| object.get("install"));
    scripts.install = install.and_then(|raw| parse_script(raw, warnings));
    scripts.update = object
        .get("update")
        .and_then(|raw| parse_script(raw, warnings));
    scripts
}

/// Parse one legacy script entry (`{script, container,
/// entrypoint}`).
fn parse_script(raw: &Value, warnings: &mut Vec<String>) -> Option<EggScript> {
    let object = raw.as_object()?;
    let script = object
        .get("script")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if script.trim().is_empty() {
        warnings.push("a script block has an empty body; skipped".to_owned());
        return None;
    }
    let container_text = object
        .get("container")
        .and_then(Value::as_str)
        .unwrap_or("debian:12-slim")
        .trim()
        .to_owned();
    let container = match container_text.parse::<DockerImage>() {
        Ok(image) => image,
        Err(e) => {
            warnings.push(format!(
                "the install container {container_text:?} is malformed; \
                 defaulting to debian:12-slim: {e}"
            ));
            "debian:12-slim"
                .parse()
                .expect("the fallback image is well-formed")
        }
    };
    let entrypoint = object
        .get("entrypoint")
        .and_then(Value::as_str)
        .unwrap_or("bash")
        .to_owned();
    Some(EggScript {
        container,
        entrypoint,
        script: script.to_owned(),
    })
}

/// Map the legacy `config` block: `files` (a JSON string or an
/// object of per-file parser rules), `startup.done`, and `stop`.
fn parse_config(raw: Option<&Value>, warnings: &mut Vec<String>) -> zim_engine_domain::EggConfig {
    let mut config = zim_engine_domain::EggConfig::default();
    let Some(object) = raw.and_then(Value::as_object) else {
        return config;
    };

    // `files` arrives either as a JSON *string* (the panel's
    // double-encoded column) or as a proper object.
    let files_value: Option<Value> = match object.get("files") {
        Some(Value::String(text)) => match serde_json::from_str::<Value>(text) {
            Ok(value) => Some(value),
            Err(e) => {
                warnings.push(format!("config.files is not valid JSON; ignored: {e}"));
                None
            }
        },
        Some(value @ Value::Object(_)) => Some(value.clone()),
        _ => None,
    };
    if let Some(files) = files_value.and_then(|value| value.as_object().cloned()) {
        for (file, rule) in files {
            if let Some(parsed) = parse_file_rule(&file, &rule, warnings) {
                config.files.push(parsed);
            }
        }
    }

    config.startup_done = object
        .get("startup")
        .and_then(|startup| startup.get("done"))
        .and_then(Value::as_str)
        .map(str::to_owned);

    if let Some(stop) = object.get("stop").and_then(Value::as_object) {
        let command = stop
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("stop")
            .to_owned();
        let signal = stop.get("signal").and_then(Value::as_bool).unwrap_or(false);
        config.stop = Some(StopConfig { command, signal });
    }
    config
}

/// Parse one per-file parser rule.
fn parse_file_rule(file: &str, rule: &Value, warnings: &mut Vec<String>) -> Option<FileParserRule> {
    let object = rule.as_object()?;
    let parser = match object
        .get("parser")
        .and_then(Value::as_str)
        .unwrap_or("file")
    {
        "properties" => ParserKind::Properties,
        "file" => ParserKind::File,
        "ini" => ParserKind::Ini,
        "yaml" => ParserKind::Yaml,
        "json" => ParserKind::Json,
        "xml" => ParserKind::Xml,
        other => {
            warnings.push(format!(
                "file {file:?} uses the unknown parser {other:?}; preserved verbatim"
            ));
            ParserKind::Other(other.to_owned())
        }
    };
    let mut replace = Vec::new();
    if let Some(entries) = object.get("replace").and_then(Value::as_array) {
        for entry in entries {
            let Some(entry) = entry.as_object() else {
                warnings.push(format!(
                    "file {file:?} has a non-object replace entry; skipped"
                ));
                continue;
            };
            // `match` (the documented key) or `target` (seen in the
            // wild) name the needle.
            let needle = entry
                .get("match")
                .or_else(|| entry.get("target"))
                .and_then(Value::as_str);
            let value = entry.get("value").and_then(Value::as_str);
            match (needle, value) {
                (Some(r#match), Some(value)) => replace.push(ReplaceRule {
                    r#match: r#match.to_owned(),
                    value: value.to_owned(),
                }),
                _ => warnings.push(format!(
                    "file {file:?} has a replace entry without a match/value pair; skipped"
                )),
            }
        }
    }
    Some(FileParserRule {
        file: file.to_owned(),
        parser,
        replace,
    })
}

/// Parse one legacy variable entry.
fn parse_variable(entry: &Value) -> Result<EggVariable, String> {
    let object = entry
        .as_object()
        .ok_or_else(|| "not an object".to_owned())?;
    let env_name = object
        .get("env_variable")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if env_name.is_empty() {
        return Err("no env_variable".to_owned());
    }
    let variable = EggVariable {
        id: EggVariableId::new(),
        env_name,
        name: object
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        description: object
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        default_value: object
            .get("default_value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        user_viewable: object
            .get("user_viewable")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        user_editable: object
            .get("user_editable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        rules: object
            .get("rules")
            .and_then(Value::as_array)
            .map(|rules| {
                rules
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
    };
    variable.validate().map_err(|e| format!("{e}"))?;
    Ok(variable)
}
