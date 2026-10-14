//! The ZIM-native `zim.toml` server-definition format.
//!
//! The typed, preferred format for new eggs
//! (MIGRATION_AND_COMPATIBILITY.md §3.2): variables are typed rows,
//! install scripts are explicitly scoped, permissions are declared.
//! The importer accepts the same [`ImportPolicy`] security gates as
//! the legacy importer; [`to_zim_toml`] exports an [`Egg`] back
//! into the format (round-trip fidelity is a test invariant).

use chrono::Utc;
use serde::{Deserialize, Serialize};

use zim_engine_domain::value::DockerImage;
use zim_engine_domain::{
    CommandTemplate, Egg, EggConfig, EggFeatures, EggId, EggImage, EggScript, EggScripts,
    EggVariable, EggVariableId, FileParserRule, ParserKind, ReplaceRule, StopConfig,
};

use crate::{EggImportError, ImportPolicy, ImportReport};

/// The format version this build reads and writes.
const FORMAT_VERSION: u32 = 1;

/// The document root (`[zim]` + `[egg]`).
#[derive(Debug, Serialize, Deserialize)]
struct ZimEggFile {
    /// The format marker.
    zim: ZimHeader,
    /// The egg definition.
    egg: ZimEgg,
}

/// The `[zim]` header.
#[derive(Debug, Serialize, Deserialize)]
struct ZimHeader {
    /// The format version (must equal [`FORMAT_VERSION`]).
    format: u32,
}

/// The `[egg]` table.
#[derive(Debug, Serialize, Deserialize)]
struct ZimEgg {
    /// Display name.
    name: String,
    /// Free-form description.
    #[serde(default)]
    description: String,
    /// Author attribution.
    #[serde(default)]
    author: String,
    /// The legacy Pterodactyl UUID (optional; deduplication).
    #[serde(default)]
    legacy_uuid: Option<String>,
    /// The startup command template.
    startup: String,
    /// The labeled images.
    #[serde(default)]
    images: Vec<ImageToml>,
    /// The feature flags.
    #[serde(default)]
    features: EggFeatures,
    /// The config rules.
    #[serde(default)]
    config: ConfigToml,
    /// The scripts.
    #[serde(default)]
    scripts: ScriptsToml,
    /// The declared variables.
    #[serde(default)]
    variables: Vec<VariableToml>,
}

/// One `[[egg.images]]` entry.
#[derive(Debug, Serialize, Deserialize)]
struct ImageToml {
    /// The image's label.
    label: String,
    /// The image reference.
    image: String,
}

/// The `[egg.config]` table.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ConfigToml {
    /// The "workload is up" console regex.
    #[serde(default)]
    startup_done: Option<String>,
    /// The stop behavior.
    #[serde(default)]
    stop: Option<StopToml>,
    /// The per-file parser rules.
    #[serde(default)]
    files: Vec<FileRuleToml>,
}

/// The `[egg.config.stop]` table.
#[derive(Debug, Serialize, Deserialize)]
struct StopToml {
    /// The stop command.
    command: String,
    /// Whether the command is a signal rather than console input.
    #[serde(default)]
    signal: bool,
}

/// One `[[egg.config.files]]` entry.
#[derive(Debug, Serialize, Deserialize)]
struct FileRuleToml {
    /// The file path relative to the data directory.
    file: String,
    /// The parser kind (`properties`, `file`, `ini`, `yaml`,
    /// `json`, `xml`; anything else is preserved verbatim).
    parser: String,
    /// The find/replace pairs.
    #[serde(default)]
    replace: Vec<ReplaceToml>,
}

/// One `[[egg.config.files.replace]]` entry.
#[derive(Debug, Serialize, Deserialize)]
struct ReplaceToml {
    /// What to match.
    r#match: String,
    /// The replacement template.
    value: String,
}

/// The `[egg.scripts]` table.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ScriptsToml {
    /// The install script.
    #[serde(default)]
    install: Option<ScriptToml>,
    /// The update script.
    #[serde(default)]
    update: Option<ScriptToml>,
}

/// One script (`[egg.scripts.install]` / `[egg.scripts.update]`).
#[derive(Debug, Serialize, Deserialize)]
struct ScriptToml {
    /// The constrained container image.
    container: String,
    /// The entrypoint.
    #[serde(default = "default_entrypoint")]
    entrypoint: String,
    /// The script body.
    script: String,
}

/// One `[[egg.variables]]` entry.
#[derive(Debug, Serialize, Deserialize)]
struct VariableToml {
    /// The env var name (uppercase).
    env_name: String,
    /// Display name.
    name: String,
    /// Free-form description.
    #[serde(default)]
    description: String,
    /// The default value.
    #[serde(default)]
    default_value: String,
    /// Whether the variable is shown to users.
    #[serde(default = "default_true")]
    user_viewable: bool,
    /// Whether users may override it.
    #[serde(default)]
    user_editable: bool,
    /// The validation rules.
    #[serde(default)]
    rules: Vec<String>,
}

fn default_entrypoint() -> String {
    "bash".to_owned()
}

fn default_true() -> bool {
    true
}

/// Import a `zim.toml` document (see the module docs).
pub fn import_zim_toml(
    source: &str,
    policy: &ImportPolicy,
) -> Result<ImportReport, EggImportError> {
    let file: ZimEggFile = toml::from_str(source)
        .map_err(|e| EggImportError::Toml(format!("not a valid zim.toml document: {e}")))?;
    if file.zim.format != FORMAT_VERSION {
        return Err(EggImportError::Toml(format!(
            "unsupported zim.toml format version {} (this build reads {FORMAT_VERSION})",
            file.zim.format
        )));
    }

    if file.egg.features.privileged_install_scripts && !policy.allow_privileged_install_scripts {
        return Err(EggImportError::PrivilegedInstallScripts);
    }
    if file.egg.features.requires_root && !policy.allow_requires_root {
        return Err(EggImportError::RequiresRoot);
    }

    let mut warnings = Vec::new();
    let images = file
        .egg
        .images
        .iter()
        .filter_map(|image| match image.image.parse::<DockerImage>() {
            Ok(parsed) => Some(EggImage {
                label: image.label.clone(),
                image: parsed,
            }),
            Err(e) => {
                warnings.push(format!(
                    "image {:?} ({:?}) is malformed; skipped: {e}",
                    image.label, image.image
                ));
                None
            }
        })
        .collect::<Vec<_>>();
    if images.is_empty() {
        warnings.push(
            "the definition declares no usable docker image; it imports, but instances cannot \
             be created from it until an image is added"
                .to_owned(),
        );
    }

    let scripts = EggScripts {
        install: file.egg.scripts.install.as_ref().map(|script| EggScript {
            container: script.container.parse().unwrap_or_else(|_| {
                warnings.push(format!(
                    "the install container {:?} is malformed; defaulting to debian:12-slim",
                    script.container
                ));
                "debian:12-slim"
                    .parse()
                    .expect("the fallback image is well-formed")
            }),
            entrypoint: script.entrypoint.clone(),
            script: script.script.clone(),
        }),
        update: file.egg.scripts.update.as_ref().map(|script| EggScript {
            container: script.container.parse().unwrap_or_else(|_| {
                "debian:12-slim"
                    .parse()
                    .expect("the fallback image is well-formed")
            }),
            entrypoint: script.entrypoint.clone(),
            script: script.script.clone(),
        }),
    };

    let config = EggConfig {
        files: file
            .egg
            .config
            .files
            .iter()
            .map(|rule| FileParserRule {
                file: rule.file.clone(),
                parser: parse_parser_kind(&rule.parser, &rule.file, &mut warnings),
                replace: rule
                    .replace
                    .iter()
                    .map(|pair| ReplaceRule {
                        r#match: pair.r#match.clone(),
                        value: pair.value.clone(),
                    })
                    .collect(),
            })
            .collect(),
        startup_done: file.egg.config.startup_done.clone(),
        stop: file.egg.config.stop.as_ref().map(|stop| StopConfig {
            command: stop.command.clone(),
            signal: stop.signal,
        }),
    };

    let variables = file
        .egg
        .variables
        .iter()
        .map(|variable| EggVariable {
            id: EggVariableId::new(),
            env_name: variable.env_name.clone(),
            name: variable.name.clone(),
            description: variable.description.clone(),
            default_value: variable.default_value.clone(),
            user_viewable: variable.user_viewable,
            user_editable: variable.user_editable,
            rules: variable.rules.clone(),
        })
        .collect::<Vec<_>>();

    let startup = CommandTemplate::new(file.egg.startup.clone());
    let intrinsics = ["SERVER_MEMORY", "SERVER_IP", "SERVER_PORT"];
    let declared: Vec<&str> = variables.iter().map(|v| v.env_name.as_str()).collect();
    for name in startup.variables() {
        if !intrinsics.contains(&name.as_str()) && !declared.contains(&name.as_str()) {
            warnings.push(format!(
                "the startup template references {name:?}, which is neither an intrinsic \
                 ({intrinsics:?}) nor a declared variable"
            ));
        }
    }

    let now = Utc::now();
    let egg = Egg {
        id: EggId::new(),
        legacy_uuid: file.egg.legacy_uuid,
        name: file.egg.name.trim().to_owned(),
        description: file.egg.description,
        author: file.egg.author,
        images,
        startup,
        features: file.egg.features,
        scripts,
        config,
        variables,
        created_at: now,
        updated_at: now,
    };
    egg.validate().map_err(EggImportError::Domain)?;
    Ok(ImportReport { egg, warnings })
}

/// Export an [`Egg`] into the `zim.toml` format (round-trips
/// through [`import_zim_toml`] with the same policy).
pub fn to_zim_toml(egg: &Egg) -> Result<String, EggImportError> {
    let file = ZimEggFile {
        zim: ZimHeader {
            format: FORMAT_VERSION,
        },
        egg: ZimEgg {
            name: egg.name.clone(),
            description: egg.description.clone(),
            author: egg.author.clone(),
            legacy_uuid: egg.legacy_uuid.clone(),
            startup: egg.startup.as_str().to_owned(),
            images: egg
                .images
                .iter()
                .map(|image| ImageToml {
                    label: image.label.clone(),
                    image: image.image.to_string(),
                })
                .collect(),
            features: egg.features,
            config: ConfigToml {
                startup_done: egg.config.startup_done.clone(),
                stop: egg.config.stop.as_ref().map(|stop| StopToml {
                    command: stop.command.clone(),
                    signal: stop.signal,
                }),
                files: egg
                    .config
                    .files
                    .iter()
                    .map(|rule| FileRuleToml {
                        file: rule.file.clone(),
                        parser: parser_kind_string(&rule.parser).to_owned(),
                        replace: rule
                            .replace
                            .iter()
                            .map(|pair| ReplaceToml {
                                r#match: pair.r#match.clone(),
                                value: pair.value.clone(),
                            })
                            .collect(),
                    })
                    .collect(),
            },
            scripts: ScriptsToml {
                install: egg.scripts.install.as_ref().map(|script| ScriptToml {
                    container: script.container.to_string(),
                    entrypoint: script.entrypoint.clone(),
                    script: script.script.clone(),
                }),
                update: egg.scripts.update.as_ref().map(|script| ScriptToml {
                    container: script.container.to_string(),
                    entrypoint: script.entrypoint.clone(),
                    script: script.script.clone(),
                }),
            },
            variables: egg
                .variables
                .iter()
                .map(|variable| VariableToml {
                    env_name: variable.env_name.clone(),
                    name: variable.name.clone(),
                    description: variable.description.clone(),
                    default_value: variable.default_value.clone(),
                    user_viewable: variable.user_viewable,
                    user_editable: variable.user_editable,
                    rules: variable.rules.clone(),
                })
                .collect(),
        },
    };
    toml::to_string_pretty(&file)
        .map_err(|e| EggImportError::Toml(format!("cannot serialize the egg: {e}")))
}

/// Map a parser-kind string, warning on unknown kinds.
fn parse_parser_kind(raw: &str, file: &str, warnings: &mut Vec<String>) -> ParserKind {
    match raw {
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
    }
}

/// The canonical string for a parser kind (the export side of
/// [`parse_parser_kind`]).
fn parser_kind_string(kind: &ParserKind) -> &str {
    match kind {
        ParserKind::Properties => "properties",
        ParserKind::File => "file",
        ParserKind::Ini => "ini",
        ParserKind::Yaml => "yaml",
        ParserKind::Json => "json",
        ParserKind::Xml => "xml",
        ParserKind::Other(other) => other,
    }
}
