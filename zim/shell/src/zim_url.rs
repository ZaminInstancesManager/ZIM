//! The `zim://` namespace → engine-origin mapping (AD-22/AD-23).
//!
//! `zim://` URLs are ZIM's **stable identifiers** — the persistence
//! format for sessions, quick access, split pairings, and extension
//! links (BROWSER_INTEGRATION.md §3). They are *not* loadable URLs:
//! at navigation time the shell maps one onto the engine's loopback
//! origin, where the engine's frontend loader serves the same
//! surfaces under `/zim/…` (the M11 page surface).
//!
//! Why mapping instead of a custom `zim://` protocol handler: the
//! frontend package is same-origin by design (HttpOnly session
//! cookie + WebSocket sub-protocol); a synthetic origin would break
//! both, and the architecture forbids the shell proxying engine
//! traffic (§2). See AD-23 for the full rationale.

#![forbid(unsafe_code)]

/// Why a `zim://` URL could not be mapped.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ZimUrlError {
    /// Not a `zim://` URL at all.
    #[error("not a zim:// URL: {0:?}")]
    NotZim(String),
    /// The path carries characters outside the safe surface set.
    #[error("unsafe zim:// path: {0:?}")]
    UnsafePath(String),
    /// The path is not empty but has no non-empty first segment.
    #[error("empty zim:// surface")]
    EmptySurface,
}

/// Map a `zim://` URL onto the engine's loopback origin.
///
/// Rules (the URL inventory in BROWSER_INTEGRATION.md §3):
///
/// - `zim://newtab` — and the bare `zim://` — map to the engine's
///   page root `/zim/`, which the engine routes to the active
///   frontend package's newtab entrypoint (the NTP-replacement
///   rule, §8).
/// - Every other surface maps to `/zim/{path}` verbatim:
///   `zim://servers/{id}/console` → `/zim/servers/{id}/console`.
/// - The scheme match is case-insensitive (`ZIM://newtab` works,
///   like browsers); the surface path is used as given.
/// - Paths are confined to `[A-Za-z0-9._~/-]` — no `?`, `#`, `%`,
///   spaces, dot-dot traversal, or control characters. Anything
///   else is rejected (`UnsafePath`) rather than sanitized: the
///   caller (address bar, session restore, extension link) should
///   show the user a bad-URL error, not a guess.
pub fn resolve(zim_url: &str, origin: &str) -> Result<String, ZimUrlError> {
    let rest = strip_scheme(zim_url).ok_or_else(|| ZimUrlError::NotZim(zim_url.to_owned()))?;

    // Trim slashes; the first segment is the surface id.
    let path = rest.trim_matches('/');
    if path.is_empty() {
        // The bare namespace routes to the page root → newtab.
        return Ok(format!("{origin}/zim/"));
    }

    if !is_safe_path(path) {
        return Err(ZimUrlError::UnsafePath(zim_url.to_owned()));
    }

    if path == "newtab" {
        // The engine's page root IS the newtab entrypoint.
        return Ok(format!("{origin}/zim/"));
    }

    Ok(format!("{origin}/zim/{path}"))
}

/// The canonical display form (what the M22 address bar shows):
/// scheme lowercased, trailing slashes trimmed.
pub fn canonical(zim_url: &str) -> Option<String> {
    let rest = strip_scheme(zim_url)?;
    let path = rest.trim_matches('/');
    Some(format!("zim://{path}"))
}

/// Strip a case-insensitive `zim://` prefix.
fn strip_scheme(zim_url: &str) -> Option<&str> {
    let (scheme, rest) = zim_url.split_once("://")?;
    if scheme.eq_ignore_ascii_case("zim") {
        Some(rest)
    } else {
        None
    }
}

/// The path-safety predicate (see [`resolve`]).
fn is_safe_path(path: &str) -> bool {
    !path.is_empty()
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'/' | b'-'))
        && !path.split('/').any(|segment| segment == "..")
        && !path.starts_with('/')
        && !path.ends_with('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: &str = "https://127.0.0.1:8443";

    #[test]
    fn the_full_url_inventory_maps() {
        // BROWSER_INTEGRATION.md §3, row by row.
        let cases = [
            ("zim://newtab", format!("{ORIGIN}/zim/")),
            // M22: the application chrome surface (tab strip +
            // omnibox hosting the panes).
            ("zim://app", format!("{ORIGIN}/zim/app")),
            ("zim://servers", format!("{ORIGIN}/zim/servers")),
            (
                "zim://servers/9f2c/console",
                format!("{ORIGIN}/zim/servers/9f2c/console"),
            ),
            (
                "zim://servers/9f2c/files",
                format!("{ORIGIN}/zim/servers/9f2c/files"),
            ),
            (
                "zim://servers/9f2c/settings",
                format!("{ORIGIN}/zim/servers/9f2c/settings"),
            ),
            (
                "zim://servers/9f2c/backups",
                format!("{ORIGIN}/zim/servers/9f2c/backups"),
            ),
            (
                "zim://servers/9f2c/schedules",
                format!("{ORIGIN}/zim/servers/9f2c/schedules"),
            ),
            (
                "zim://servers/9f2c/subusers",
                format!("{ORIGIN}/zim/servers/9f2c/subusers"),
            ),
            (
                "zim://servers/9f2c/metrics",
                format!("{ORIGIN}/zim/servers/9f2c/metrics"),
            ),
            (
                "zim://workloads/minecraft/new",
                format!("{ORIGIN}/zim/workloads/minecraft/new"),
            ),
            ("zim://extensions", format!("{ORIGIN}/zim/extensions")),
            ("zim://settings", format!("{ORIGIN}/zim/settings")),
            (
                "zim://settings/servers/9f2c",
                format!("{ORIGIN}/zim/settings/servers/9f2c"),
            ),
            ("zim://recovery", format!("{ORIGIN}/zim/recovery")),
        ];
        for (zim, expected) in cases {
            assert_eq!(resolve(zim, ORIGIN).unwrap(), expected, "mapping {zim}");
        }
    }

    #[test]
    fn bare_and_rooted_namespace_is_newtab() {
        assert_eq!(resolve("zim://", ORIGIN).unwrap(), format!("{ORIGIN}/zim/"));
        assert_eq!(
            resolve("zim:///", ORIGIN).unwrap(),
            format!("{ORIGIN}/zim/")
        );
        assert_eq!(
            resolve("zim://newtab/", ORIGIN).unwrap(),
            format!("{ORIGIN}/zim/")
        );
    }

    #[test]
    fn scheme_is_case_insensitive() {
        assert_eq!(
            resolve("ZIM://servers", ORIGIN).unwrap(),
            format!("{ORIGIN}/zim/servers")
        );
    }

    #[test]
    fn other_schemes_are_rejected() {
        assert_eq!(
            resolve("https://127.0.0.1:8443/zim/", ORIGIN),
            Err(ZimUrlError::NotZim(
                "https://127.0.0.1:8443/zim/".to_owned()
            ))
        );
        assert_eq!(
            resolve("http://example.test/", ORIGIN),
            Err(ZimUrlError::NotZim("http://example.test/".to_owned()))
        );
        assert!(resolve("about:blank", ORIGIN).is_err());
    }

    #[test]
    fn traversal_and_special_characters_are_rejected() {
        for bad in [
            "zim://../etc/passwd",
            "zim://servers/../../x",
            "zim://servers?x=1",
            "zim://servers#fragment",
            "zim://servers/%2e%2e",
            "zim://servers/with space",
            "zim://servers/with\"quote",
            "zim://servers/semi;colon",
        ] {
            assert!(
                matches!(resolve(bad, ORIGIN), Err(ZimUrlError::UnsafePath(_))),
                "expected UnsafePath for {bad}"
            );
        }
    }

    #[test]
    fn canonical_trims_and_lowercases() {
        assert_eq!(
            canonical("ZIM://servers/").as_deref(),
            Some("zim://servers")
        );
        assert_eq!(canonical("zim://newtab").as_deref(), Some("zim://newtab"));
        assert_eq!(canonical("https://elsewhere/"), None);
    }
}
