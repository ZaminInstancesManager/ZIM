//! Support value types shared across domain entities.
//!
//! These are typed wrappers over primitives that carry validation
//! and domain semantics: `Bytes` (byte quantities), `CommandTemplate`
//! (startup command with `{{VAR}}` substitution), `DockerImage`
//! (parsed and validated image reference), and `Scheme` (http/https).
//!
//! # Source of truth
//!
//! These types appear as field types in the entity structs of
//! `docs/architecture/RUST_ENGINE.md` §3 (`ResourceLimits.memory:
//! Bytes`, `Instance.startup: CommandTemplate`, `Instance.image:
//! DockerImage`, `Node.scheme: Scheme`). The reference semantics for
//! `CommandTemplate` substitution come from Pterodactyl's startup
//! command templating (`{{SERVER_MEMORY}}` et al., verified in
//! `docs/audits/PTERODACTYL_ARCHITECTURE.md`), preserved per AD-12.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

// ─── Bytes ────────────────────────────────────────────────────────────────

/// A byte quantity (memory / swap / disk limits).
///
/// A newtype over `u64` so that limit arithmetic and comparisons are
/// explicitly typed. Serializes transparently as a JSON number.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(transparent)]
pub struct Bytes(pub u64);

impl Bytes {
    /// Construct from a raw byte count.
    pub const fn from_bytes(n: u64) -> Self {
        Self(n)
    }

    /// Construct from mebibytes (MiB), matching Pterodactyl's
    /// memory-limit units.
    pub const fn from_mib(n: u64) -> Self {
        Self(n * 1024 * 1024)
    }

    /// Construct from gibibytes (GiB).
    pub const fn from_gib(n: u64) -> Self {
        Self(n * 1024 * 1024 * 1024)
    }

    /// The raw byte count.
    pub const fn as_bytes(&self) -> u64 {
        self.0
    }

    /// The quantity in mebibytes (truncated).
    pub const fn as_mib(&self) -> u64 {
        self.0 / (1024 * 1024)
    }

    /// The quantity in gibibytes (truncated).
    pub const fn as_gib(&self) -> u64 {
        self.0 / (1024 * 1024 * 1024)
    }
}

impl fmt::Display for Bytes {
    /// Human-readable form with binary (IEC) unit suffixes, e.g.
    /// `512 MiB`, `2 GiB`, `768 KiB`, `13 B`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const KIB: u64 = 1024;
        const MIB: u64 = KIB * 1024;
        const GIB: u64 = MIB * 1024;
        const TIB: u64 = GIB * 1024;

        let n = self.0;
        if n >= TIB {
            write!(f, "{} TiB", n / TIB)
        } else if n >= GIB {
            write!(f, "{} GiB", n / GIB)
        } else if n >= MIB {
            write!(f, "{} MiB", n / MIB)
        } else if n >= KIB {
            write!(f, "{} KiB", n / KIB)
        } else {
            write!(f, "{} B", n)
        }
    }
}

impl From<u64> for Bytes {
    fn from(n: u64) -> Self {
        Self(n)
    }
}

// ─── Scheme ───────────────────────────────────────────────────────────────

/// URL scheme for reaching a remote node's contract endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Scheme {
    /// Plain HTTP (loopback or trusted-network deployments only).
    Http,
    /// HTTPS (the default; secure-by-default per the API spec's
    /// transport requirements).
    #[default]
    Https,
}

impl Scheme {
    /// The canonical string form.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}

impl fmt::Display for Scheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Scheme {
    type Err = SchemeParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "http" => Ok(Self::Http),
            "https" => Ok(Self::Https),
            other => Err(SchemeParseError(other.to_owned())),
        }
    }
}

/// Error returned when a scheme string is neither `http` nor `https`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown scheme {0:?}; expected \"http\" or \"https\"")]
pub struct SchemeParseError(String);

// ─── CommandTemplate ──────────────────────────────────────────────────────

/// A startup command template with `{{VARIABLE}}` placeholders.
///
/// Pterodactyl eggs define startup commands like
/// `java -Xms128M -Xmx{{SERVER_MEMORY}}M -jar {{SERVER_JAR}}`, where
/// the placeholders are filled from the egg's variable defaults plus
/// per-instance overrides. ZIM preserves that templating shape
/// verbatim per AD-12.
///
/// The engine renders a template by substituting every
/// `{{VARIABLE}}` placeholder with the value of the same-named
/// variable. Unknown variables are a *render error* (strict mode,
/// used before actually launching a process) or left verbatim
/// (lenient mode, used for UI previews where partial configuration
/// is expected).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommandTemplate(pub String);

/// Error produced while rendering a [`CommandTemplate`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CommandTemplateError {
    /// A `{{...}}` placeholder referenced a variable that was not
    /// supplied in the render context.
    #[error("unknown template variable {name:?}")]
    UnknownVariable {
        /// The variable name that was referenced but not supplied.
        name: String,
    },
    /// A `{{...}}` placeholder had an empty variable name.
    #[error("empty template variable name")]
    EmptyVariableName,
}

impl CommandTemplate {
    /// Construct a template from a raw string.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// The raw template string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Collect the names of every `{{VARIABLE}}` placeholder in this
    /// template, in order of first appearance.
    pub fn variables(&self) -> Vec<String> {
        let mut seen = Vec::new();
        for name in placeholder_names(&self.0) {
            if !seen.contains(&name) {
                seen.push(name);
            }
        }
        seen
    }

    /// Render the template, substituting every `{{VARIABLE}}` from
    /// `vars`.
    ///
    /// Strict mode: an unknown or empty variable name is an error.
    /// Use this before launching a process so that a mis-configured
    /// instance fails fast instead of launching with a literal
    /// `{{SERVER_JAR}}` in its command line.
    pub fn render(&self, vars: &BTreeMap<String, String>) -> Result<String, CommandTemplateError> {
        render_impl(&self.0, vars, true)
    }

    /// Render the template, leaving unknown placeholders verbatim.
    ///
    /// Lenient mode: for UI previews where the user is still filling
    /// in variables, unknown placeholders stay visible so the preview
    /// shows what still needs configuration.
    pub fn render_lenient(&self, vars: &BTreeMap<String, String>) -> String {
        // Lenient rendering cannot fail by construction: unknown
        // variables are kept verbatim and empty names are kept as
        // the literal `{{}}`.
        render_impl(&self.0, vars, false).unwrap_or_else(|_| self.0.clone())
    }
}

impl fmt::Display for CommandTemplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Splits a template string into literal segments and placeholders.
///
/// A placeholder starts with `{{` and ends at the *first* subsequent
/// `}}`. Nested `{{{{` is treated as a literal `{{` followed by the
/// start of a placeholder scan continuing after it.
fn tokenize(template: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let (literal, after) = rest.split_at(start);
        let after = &after[2..];
        if let Some(end) = after.find("}}") {
            let name = &after[..end];
            if !literal.is_empty() {
                out.push((literal.to_owned(), None));
            }
            out.push((name.to_owned(), Some(name.to_owned())));
            rest = &after[end + 2..];
        } else {
            // No closing braces: the rest is literal.
            out.push((rest.to_owned(), None));
            return out;
        }
    }
    if !rest.is_empty() {
        out.push((rest.to_owned(), None));
    }
    out
}

/// Iterates placeholder variable names in order of appearance.
fn placeholder_names(template: &str) -> impl Iterator<Item = String> + '_ {
    tokenize(template)
        .into_iter()
        .filter_map(|(_, placeholder)| placeholder)
}

fn render_impl(
    template: &str,
    vars: &BTreeMap<String, String>,
    strict: bool,
) -> Result<String, CommandTemplateError> {
    let mut rendered = String::with_capacity(template.len());
    for (segment, placeholder) in tokenize(template) {
        match placeholder {
            None => rendered.push_str(&segment),
            Some(name) => {
                if name.is_empty() {
                    if strict {
                        return Err(CommandTemplateError::EmptyVariableName);
                    }
                    rendered.push_str("{{}}");
                } else if let Some(value) = vars.get(&name) {
                    rendered.push_str(value);
                } else if strict {
                    return Err(CommandTemplateError::UnknownVariable { name });
                } else {
                    rendered.push_str("{{");
                    rendered.push_str(&name);
                    rendered.push_str("}}");
                }
            }
        }
    }
    Ok(rendered)
}

// ─── DockerImage ──────────────────────────────────────────────────────────

/// A parsed and validated Docker image reference.
///
/// Grammar (matching the Docker distribution reference grammar,
/// simplified to the subset ZIM accepts in egg Docker images):
///
/// ```text
/// reference := [registry "/"] repository [":" tag]
/// ```
///
/// - `registry` is present when the first path component contains a
///   `.` or `:` or equals `localhost` (e.g. `ghcr.io`,
///   `registry.internal:5000`). Otherwise there is no registry and
///   the whole path is the repository (Docker Hub naming).
/// - `repository` components match `[a-z0-9]+(?:(?:[._]|__|[-]*)[a-z0-9]+)*`
///   (lowercase only, per the distribution spec).
/// - `tag` is `[A-Za-z0-9_][A-Za-z0-9._-]{0,127}` and is non-empty
///   when the `:` separator is present. When omitted, the canonical
///   form serializes with the default tag `latest`.
///
/// Typical egg images look like `ghcr.io/parkervcp/yolks:java_21`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DockerImage {
    registry: Option<String>,
    repository: String,
    tag: String,
}

/// Error produced while parsing a [`DockerImage`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DockerImageParseError {
    /// The reference was empty or only whitespace.
    #[error("empty image reference")]
    Empty,
    /// The repository failed the component charset/shape rules.
    #[error("invalid repository {0:?}")]
    InvalidRepository(String),
    /// The registry failed the host validity rules.
    #[error("invalid registry {0:?}")]
    InvalidRegistry(String),
    /// The tag failed the charset/length rules, or was empty after
    /// the `:` separator.
    #[error("invalid tag {0:?}")]
    InvalidTag(String),
}

impl DockerImage {
    /// The registry host, if any (`ghcr.io`, `localhost:5000`, ...).
    pub fn registry(&self) -> Option<&str> {
        self.registry.as_deref()
    }

    /// The repository path (e.g. `parkervcp/yolks` or `library/ubuntu`).
    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// The tag (never empty; `latest` when the reference omitted it).
    pub fn tag(&self) -> &str {
        &self.tag
    }
}

impl FromStr for DockerImage {
    type Err = DockerImageParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err(DockerImageParseError::Empty);
        }

        // Split off the tag at the LAST ':' (tags may not contain
        // '/', so a ':' after the final '/' is the tag separator; a
        // ':' before any '/' belongs to a registry port).
        let (name_part, tag) = match s.rfind(':') {
            Some(idx) if !s[idx + 1..].contains('/') => {
                let tag = &s[idx + 1..];
                if !is_valid_tag(tag) {
                    return Err(DockerImageParseError::InvalidTag(tag.to_owned()));
                }
                (&s[..idx], tag)
            }
            _ => (s, "latest"),
        };

        // Split off the registry when the first component looks like
        // a host (contains '.' or ':', or is exactly "localhost").
        let (registry, repository) = match name_part.find('/') {
            Some(0) => {
                return Err(DockerImageParseError::InvalidRepository(
                    name_part.to_owned(),
                ))
            }
            Some(idx) => {
                let first = &name_part[..idx];
                if first.contains('.') || first.contains(':') || first == "localhost" {
                    let repo = &name_part[idx + 1..];
                    if repo.is_empty() {
                        return Err(DockerImageParseError::InvalidRepository(
                            name_part.to_owned(),
                        ));
                    }
                    if !is_valid_host(first) {
                        return Err(DockerImageParseError::InvalidRegistry(first.to_owned()));
                    }
                    (Some(first.to_owned()), repo)
                } else {
                    (None, name_part)
                }
            }
            None => (None, name_part),
        };

        if !is_valid_repository(repository) {
            return Err(DockerImageParseError::InvalidRepository(
                repository.to_owned(),
            ));
        }

        Ok(Self {
            registry,
            repository: repository.to_owned(),
            tag: tag.to_owned(),
        })
    }
}

fn is_valid_host(host: &str) -> bool {
    // Host: non-empty, no '/', and either host[:port] with numeric
    // port or a bare hostname with at least one dot (or localhost).
    if host.is_empty() || host.contains('/') {
        return false;
    }
    if let Some((h, port)) = host.rsplit_once(':') {
        !h.is_empty() && port.chars().all(|c| c.is_ascii_digit()) && !port.is_empty()
    } else {
        // Bare registry host must contain a dot (e.g. ghcr.io) or be
        // exactly "localhost" — enforced by the caller's split logic,
        // so here we only need to reject spaces and stray characters.
        !host.contains(' ')
    }
}

fn is_valid_repository(repo: &str) -> bool {
    if repo.is_empty() || repo.starts_with('/') || repo.ends_with('/') || repo.contains("//") {
        return false;
    }
    repo.split('/').all(is_valid_repo_component)
}

fn is_valid_repo_component(component: &str) -> bool {
    if component.is_empty() {
        return false;
    }
    // Distribution component charset: lowercase alphanumerics plus
    // separators '.', '_', '-' (with the '__' and '-'-run allowances
    // simplified to permissive charset checking, matching how most
    // validators are implemented in practice).
    component
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == '-')
}

fn is_valid_tag(tag: &str) -> bool {
    // Tag: 1..=128 chars, starts with word char, then word chars or
    // '.' '-' '_'. Uppercase IS allowed in tags (unlike repositories).
    let mut chars = tag.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() || c == '_' => {}
        _ => return false,
    }
    let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_');
    rest_ok && tag.len() <= 128
}

impl fmt::Display for DockerImage {
    /// Canonical form: `[registry/]repository:tag` — the tag is
    /// always explicit (an omitted tag serializes as `:latest`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(registry) = &self.registry {
            write!(f, "{}/{}:{}", registry, self.repository, self.tag)
        } else {
            write!(f, "{}:{}", self.repository, self.tag)
        }
    }
}

impl Serialize for DockerImage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for DockerImage {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Bytes ─────────────────────────────────────────────────────────

    #[test]
    fn bytes_round_trips_through_serde() {
        let b = Bytes::from_mib(512);
        let s = serde_json::to_string(&b).unwrap();
        assert_eq!(s, "536870912");
        let b2: Bytes = serde_json::from_str(&s).unwrap();
        assert_eq!(b, b2);
    }

    #[test]
    fn bytes_conversions_are_consistent() {
        assert_eq!(Bytes::from_mib(1).as_bytes(), 1024 * 1024);
        assert_eq!(Bytes::from_gib(2).as_mib(), 2048);
        assert_eq!(Bytes::from_bytes(768 * 1024).as_mib(), 0);
    }

    #[test]
    fn bytes_display_uses_iec_units() {
        assert_eq!(Bytes::from_bytes(13).to_string(), "13 B");
        assert_eq!(Bytes::from_bytes(768 * 1024).to_string(), "768 KiB");
        assert_eq!(Bytes::from_mib(512).to_string(), "512 MiB");
        assert_eq!(Bytes::from_gib(2).to_string(), "2 GiB");
        assert_eq!(Bytes::from_gib(1024).to_string(), "1 TiB");
    }

    // ── Scheme ────────────────────────────────────────────────────────

    #[test]
    fn scheme_round_trips_through_serde() {
        for scheme in [Scheme::Http, Scheme::Https] {
            let s = serde_json::to_string(&scheme).unwrap();
            let back: Scheme = serde_json::from_str(&s).unwrap();
            assert_eq!(scheme, back);
        }
        assert_eq!(serde_json::to_string(&Scheme::Http).unwrap(), "\"http\"");
        assert_eq!(serde_json::to_string(&Scheme::Https).unwrap(), "\"https\"");
    }

    #[test]
    fn scheme_default_is_https() {
        assert_eq!(Scheme::default(), Scheme::Https);
    }

    #[test]
    fn scheme_parses_and_rejects() {
        assert_eq!("https".parse::<Scheme>().unwrap(), Scheme::Https);
        assert_eq!("http".parse::<Scheme>().unwrap(), Scheme::Http);
        assert!("ftp".parse::<Scheme>().is_err());
    }

    // ── CommandTemplate ───────────────────────────────────────────────

    #[test]
    fn template_lists_variables_in_first_appearance_order() {
        let t = CommandTemplate::new(
            "java -Xmx{{SERVER_MEMORY}}M -jar {{SERVER_JAR}} {{SERVER_MEMORY}}",
        );
        assert_eq!(
            t.variables(),
            vec!["SERVER_MEMORY".to_string(), "SERVER_JAR".to_string()]
        );
    }

    #[test]
    fn template_renders_known_variables() {
        let t = CommandTemplate::new("java -Xms128M -Xmx{{SERVER_MEMORY}}M -jar {{SERVER_JAR}}");
        let mut vars = BTreeMap::new();
        vars.insert("SERVER_MEMORY".to_string(), "1024".to_string());
        vars.insert("SERVER_JAR".to_string(), "server.jar".to_string());
        assert_eq!(
            t.render(&vars).unwrap(),
            "java -Xms128M -Xmx1024M -jar server.jar"
        );
    }

    #[test]
    fn template_strict_render_rejects_unknown_variable() {
        let t = CommandTemplate::new("run {{MISSING_VAR}}");
        let vars = BTreeMap::new();
        match t.render(&vars) {
            Err(CommandTemplateError::UnknownVariable { name }) => assert_eq!(name, "MISSING_VAR"),
            other => panic!("expected UnknownVariable, got {:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn template_strict_render_rejects_empty_name() {
        let t = CommandTemplate::new("run {{}} now");
        let vars = BTreeMap::new();
        assert!(matches!(
            t.render(&vars),
            Err(CommandTemplateError::EmptyVariableName)
        ));
    }

    #[test]
    fn template_lenient_render_keeps_unknown_verbatim() {
        let t = CommandTemplate::new("java -jar {{SERVER_JAR}} --mode {{MODE}}");
        let mut vars = BTreeMap::new();
        vars.insert("MODE".to_string(), "online".to_string());
        assert_eq!(
            t.render_lenient(&vars),
            "java -jar {{SERVER_JAR}} --mode online"
        );
    }

    #[test]
    fn template_without_variables_renders_verbatim() {
        let t = CommandTemplate::new("sleep infinity");
        assert_eq!(t.render(&BTreeMap::new()).unwrap(), "sleep infinity");
        assert!(t.variables().is_empty());
    }

    #[test]
    fn template_unclosed_brace_is_literal() {
        let t = CommandTemplate::new("echo {{OPEN");
        assert_eq!(t.render(&BTreeMap::new()).unwrap(), "echo {{OPEN");
        assert!(t.variables().is_empty());
    }

    #[test]
    fn template_round_trips_through_serde() {
        let t = CommandTemplate::new("java -Xmx{{SERVER_MEMORY}}M");
        let s = serde_json::to_string(&t).unwrap();
        let back: CommandTemplate = serde_json::from_str(&s).unwrap();
        assert_eq!(t, back);
    }

    // ── DockerImage ───────────────────────────────────────────────────

    #[test]
    fn image_parses_registry_repository_tag() {
        let img: DockerImage = "ghcr.io/parkervcp/yolks:java_21".parse().unwrap();
        assert_eq!(img.registry(), Some("ghcr.io"));
        assert_eq!(img.repository(), "parkervcp/yolks");
        assert_eq!(img.tag(), "java_21");
        assert_eq!(img.to_string(), "ghcr.io/parkervcp/yolks:java_21");
    }

    #[test]
    fn image_defaults_tag_to_latest() {
        let img: DockerImage = "ubuntu".parse().unwrap();
        assert_eq!(img.registry(), None);
        assert_eq!(img.repository(), "ubuntu");
        assert_eq!(img.tag(), "latest");
        assert_eq!(img.to_string(), "ubuntu:latest");
    }

    #[test]
    fn image_registry_with_port_does_not_confuse_tag_split() {
        let img: DockerImage = "registry.internal:5000/team/app:1.2.3".parse().unwrap();
        assert_eq!(img.registry(), Some("registry.internal:5000"));
        assert_eq!(img.repository(), "team/app");
        assert_eq!(img.tag(), "1.2.3");
    }

    #[test]
    fn image_localhost_is_registry() {
        let img: DockerImage = "localhost/myapp:dev".parse().unwrap();
        assert_eq!(img.registry(), Some("localhost"));
        assert_eq!(img.repository(), "myapp");
        assert_eq!(img.tag(), "dev");
    }

    #[test]
    fn image_rejects_uppercase_repository() {
        assert!("MyApp/app:latest".parse::<DockerImage>().is_err());
    }

    #[test]
    fn image_rejects_empty_tag() {
        assert!("ubuntu:".parse::<DockerImage>().is_err());
    }

    #[test]
    fn image_rejects_empty_reference() {
        assert!("".parse::<DockerImage>().is_err());
        assert!("   ".parse::<DockerImage>().is_err());
    }

    #[test]
    fn image_round_trips_through_serde() {
        let img: DockerImage = "ghcr.io/parkervcp/yolks:java_21".parse().unwrap();
        let s = serde_json::to_string(&img).unwrap();
        assert_eq!(s, "\"ghcr.io/parkervcp/yolks:java_21\"");
        let back: DockerImage = serde_json::from_str(&s).unwrap();
        assert_eq!(img, back);
    }

    #[test]
    fn image_serde_rejects_invalid_reference() {
        let bad = serde_json::to_string("Bad Repo!").unwrap();
        assert!(serde_json::from_str::<DockerImage>(&bad).is_err());
    }
}
