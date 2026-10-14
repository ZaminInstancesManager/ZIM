//! First-run install planning (M17): what "installed" means for a
//! GameServer workload.
//!
//! [`build_install_plan`] turns a [`GameServerSpec`] into an ordered
//! [`InstallPlan`]:
//!
//! 1. the egg's install script (skipped when the spec says so) —
//!    *referenced* by the plan; the installs job handler (M8.3,
//!    `zim_engine_installs` + the Docker install runner) executes it
//!    in its constrained container;
//! 2. the config-file rewrite rules (the egg's parser rules with
//!    variable-rendered values — `server.properties` gets its port,
//!    motd, ...);
//! 3. the EULA write, when the user accepted it and the egg's own
//!    installer does not already handle the file.
//!
//! The plan is a typed value the integration's
//! [`prepare_install`][crate::MinecraftIntegration::prepare_install]
//! hook serializes for the engine (§10.15-shaped JSON), and
//! [`apply_file_steps`] executes the file-shaped steps directly
//! against a [`Filesystem`] — the parity tests drive exactly that
//! path.
//!
//! # Parser semantics (Wings parity)
//!
//! - `properties` — the `server.properties` shape: key=value lines;
//!   a rule's `match` is the key, its rendered `value` replaces (or
//!   appends) the pair. This is the Minecraft-critical parser.
//! - `file` — whole-file regex rewriting: `match` is a regex,
//!   `value` its rendered replacement.
//! - the structured legacy kinds (`ini`, `yaml`, `json`, `xml`) fall
//!   back to the `file` regex semantics here — a documented M17
//!   simplification; the legacy Panel's own eggs overwhelmingly use
//!   `properties` and `file` (the structured kinds are a rarity in
//!   the public egg corpus, and regex substitution preserves their
//!   observable effect for the common rules).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use serde::{Deserialize, Serialize};

use zim_engine_domain::{CommandTemplate, ParserKind};
use zim_engine_files::{FileError, Filesystem};

use crate::spec::GameServerSpec;

/// One step of an install plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InstallStep {
    /// Run the egg's install script in its constrained container
    /// (executed by the installs job handler, not the file layer).
    RunEggScript {
        /// The container image the script runs in.
        container: String,
        /// The entrypoint (`bash` in virtually all legacy eggs).
        entrypoint: String,
        /// The script body, verbatim.
        script: String,
    },
    /// Write one file (the EULA write, or any egg-mandated seed
    /// file).
    WriteFile {
        /// The file's path relative to the data directory.
        path: String,
        /// The file's content.
        content: String,
    },
    /// Apply one config-file rewrite rule with rendered values.
    ApplyParserRule {
        /// The file's path relative to the data directory.
        file: String,
        /// The parser kind.
        parser: ParserKind,
        /// The find/replace pairs, values rendered.
        replace: Vec<zim_engine_domain::ReplaceRule>,
    },
}

/// An ordered install plan (§10.15-shaped when serialized).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct InstallPlan {
    /// The steps, in execution order.
    pub steps: Vec<InstallStep>,
}

/// The Minecraft EULA file's canonical content (the line Mojang's
/// server writes and reads).
pub const EULA_FILE: &str = "eula.txt";

/// Build the install plan for one GameServer spec.
pub fn build_install_plan(spec: &GameServerSpec) -> InstallPlan {
    let mut steps = Vec::new();

    // 1. The egg script, unless the workload opts out.
    if !spec.skip_egg_scripts {
        if let Some(script) = &spec.install_script {
            steps.push(InstallStep::RunEggScript {
                container: script.container.clone(),
                entrypoint: script.entrypoint.clone(),
                script: script.script.clone(),
            });
        }
    }

    // 2. The config-file rewrite rules, values rendered.
    let vars = spec.variable_map();
    for rule in &spec.config_files {
        let replace = rule
            .replace
            .iter()
            .map(|pair| zim_engine_domain::ReplaceRule {
                r#match: pair.r#match.clone(),
                value: render_lenient(&pair.value, &vars),
            })
            .collect();
        steps.push(InstallStep::ApplyParserRule {
            file: rule.file.clone(),
            parser: rule.parser.clone(),
            replace,
        });
    }

    // 3. The EULA, when the user accepted it and the egg's installer
    //    does not already write the file itself.
    if spec.eula_accepted && !spec.eula_handled_by_installer {
        steps.push(InstallStep::WriteFile {
            path: EULA_FILE.to_owned(),
            content: "eula=true\n".to_owned(),
        });
    }

    InstallPlan { steps }
}

/// Render a template leniently (unknown placeholders stay verbatim —
/// an install plan is built before every variable is necessarily
/// configured; the *startup* render stays strict).
fn render_lenient(template: &str, vars: &std::collections::BTreeMap<String, String>) -> String {
    CommandTemplate::new(template.to_owned()).render_lenient(vars)
}

/// Apply the plan's file-shaped steps (`WriteFile` +
/// `ApplyParserRule`) through a [`Filesystem`].
///
/// `RunEggScript` steps are skipped — the installs job handler owns
/// them — and counted in the return so callers can assert the
/// script step was planned.
pub fn apply_file_steps(plan: &InstallPlan, fs_api: &Filesystem) -> Result<usize, FileError> {
    let mut deferred_scripts = 0;
    for step in &plan.steps {
        match step {
            InstallStep::RunEggScript { .. } => deferred_scripts += 1,
            InstallStep::WriteFile { path, content } => {
                fs_api.write_file(path, content.as_bytes())?;
            }
            InstallStep::ApplyParserRule {
                file,
                parser,
                replace,
            } => apply_parser_rule(fs_api, file, parser, replace)?,
        }
    }
    Ok(deferred_scripts)
}

/// Apply one parser rule to one file through the file API.
fn apply_parser_rule(
    fs_api: &Filesystem,
    file: &str,
    parser: &ParserKind,
    replace: &[zim_engine_domain::ReplaceRule],
) -> Result<(), FileError> {
    // The existing content, when any (parser rules mutate what the
    // install script left behind).
    let existing: Vec<u8> = match fs_api.read_file(file) {
        Ok(bytes) => bytes,
        Err(FileError::NotFound(_)) => Vec::new(),
        Err(other) => return Err(other),
    };
    let mut text = String::from_utf8_lossy(&existing).into_owned();

    match parser {
        ParserKind::Properties => {
            for pair in replace {
                set_properties_key(&mut text, &pair.r#match, &pair.value);
            }
        }
        // The `file` parser and the structured legacy kinds (see the
        // module docs' documented simplification).
        ParserKind::File
        | ParserKind::Ini
        | ParserKind::Yaml
        | ParserKind::Json
        | ParserKind::Xml
        | ParserKind::Other(_) => {
            for pair in replace {
                if let Ok(re) = regex::Regex::new(&pair.r#match) {
                    text = re.replace_all(&text, pair.value.as_str()).into_owned();
                }
            }
        }
    }

    fs_api.write_file(file, text.as_bytes())
}

/// Set (or append) one `key=value` pair in a properties-shaped
/// text, preserving order and unrelated keys. The key match
/// ignores whitespace around the `=` separator (the legacy
/// properties parser's behavior); duplicate keys collapse to the
/// rule's value.
fn set_properties_key(text: &mut String, key: &str, value: &str) {
    let assigned = format!("{key}={value}");
    let mut replaced = false;
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        let key_part = trimmed.split('=').next().unwrap_or("").trim();
        if key_part == key && !trimmed.starts_with('#') {
            if !replaced {
                out.push(assigned.clone());
                replaced = true;
            }
        } else {
            out.push(line.to_owned());
        }
    }
    if !replaced {
        out.push(assigned);
    }
    let mut rebuilt = out.join("\n");
    rebuilt.push('\n');
    *text = rebuilt;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_fs() -> Filesystem {
        let root = std::env::temp_dir().join(format!("zim-mc-install-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create temp root");
        Filesystem::new(&root)
    }

    #[test]
    fn parser_rules_apply_through_the_real_filesystem() {
        // The full path a plan's ApplyParserRule step takes: an
        // existing file read, rewritten, and written back through
        // the §2.6-validated file API.
        let fs_api = tmp_fs();
        fs_api
            .write_file("server.properties", b"motd=old\n")
            .expect("seed properties");

        apply_parser_rule(
            &fs_api,
            "server.properties",
            &ParserKind::Properties,
            &[
                zim_engine_domain::ReplaceRule {
                    r#match: "server-port".to_owned(),
                    value: "25565".to_owned(),
                },
                zim_engine_domain::ReplaceRule {
                    r#match: "motd".to_owned(),
                    value: "new".to_owned(),
                },
            ],
        )
        .expect("apply");

        let rewritten = String::from_utf8(fs_api.read_file("server.properties").unwrap()).unwrap();
        assert!(rewritten.contains("motd=new"));
        assert!(rewritten.contains("server-port=25565"));
        assert!(!rewritten.contains("motd=old"));
    }

    #[test]
    fn file_parser_rules_rewrite_by_regex() {
        let fs_api = tmp_fs();
        fs_api
            .write_file("config.yml", b"port: 1234\nhost: 0.0.0.0\n")
            .expect("seed yml");
        apply_parser_rule(
            &fs_api,
            "config.yml",
            &ParserKind::File,
            &[zim_engine_domain::ReplaceRule {
                r#match: "port: [0-9]+".to_owned(),
                value: "port: 25565".to_owned(),
            }],
        )
        .expect("apply");
        let rewritten = String::from_utf8(fs_api.read_file("config.yml").unwrap()).unwrap();
        assert!(rewritten.contains("port: 25565"));
        assert!(rewritten.contains("host: 0.0.0.0"));
    }

    #[test]
    fn the_eula_line_is_the_minecraft_canonical_form() {
        let mut text = String::new();
        set_properties_key(&mut text, "eula", "true");
        assert_eq!(text, "eula=true\n");
    }

    #[test]
    fn properties_rules_replace_and_append() {
        let mut text = String::from("motd=Hello\nonline-mode=true\n");
        set_properties_key(&mut text, "server-port", "25565");
        assert_eq!(text, "motd=Hello\nonline-mode=true\nserver-port=25565\n");

        set_properties_key(&mut text, "motd", "A ZIM Server");
        assert_eq!(
            text,
            "motd=A ZIM Server\nonline-mode=true\nserver-port=25565\n"
        );
    }

    #[test]
    fn properties_rules_preserve_spacing_keys() {
        let mut text = String::from("  enable-rcon = false\n");
        set_properties_key(&mut text, "enable-rcon", "true");
        assert!(text.contains("enable-rcon=true"));
        assert!(!text.contains("enable-rcon = false"));
    }

    #[test]
    fn properties_rules_skip_comment_lines() {
        let mut text = String::from("# server-port is the port\nserver-port=1234\n");
        set_properties_key(&mut text, "server-port", "25565");
        assert!(text.contains("# server-port is the port"));
        assert!(text.contains("server-port=25565"));
        assert!(!text.contains("server-port=1234"));
    }

    #[test]
    fn duplicate_keys_collapse_to_one() {
        let mut text = String::from("motd=one\nmotd=two\n");
        set_properties_key(&mut text, "motd", "three");
        assert_eq!(text, "motd=three\n");
    }

    #[test]
    fn the_eula_file_constant_is_the_conventional_path() {
        assert_eq!(EULA_FILE, "eula.txt");
    }
}
