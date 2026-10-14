//! Structural HTML checks for the frontend-package smoke gate.
//!
//! The engine cannot execute a page (that half of the §3.5 smoke
//! test runs in a renderer once the browser integration exists,
//! M13+). What the engine *can* verify — and does here — is that
//! each entrypoint is well-formed HTML that would actually render
//! under the `zim://` CSP (§3.8): documents with balanced
//! structure, no inline scripts, no inline styles or style
//! attributes, no inline event handlers, no `javascript:` URLs, and
//! no sub-resource references that would be blocked on load.
//!
//! These are the failures that make a custom frontend package
//! unusable the moment a browser loads it, which is exactly what the
//! activation gate exists to catch.

/// One structural finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlIssue {
    /// The violated rule (a stable, human-readable slug).
    pub rule: &'static str,
    /// Where it happened (element or attribute).
    pub at: String,
}

impl std::fmt::Display for HtmlIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.rule, self.at)
    }
}

/// Check one entrypoint document. `name` is used in error locations.
pub fn check_document(name: &str, bytes: &[u8]) -> Result<(), Vec<HtmlIssue>> {
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => {
            return Err(vec![HtmlIssue {
                rule: "utf8",
                at: format!("{name} is not valid UTF-8"),
            }])
        }
    };
    let mut issues = Vec::new();

    // Pass 1: structure. One <html> wrapping one <head> and one
    // <body>, each opened exactly once and closed exactly once.
    let tags = tokenize(text);
    let mut opens = [0usize; 3]; // html, head, body
    let mut closes = [0usize; 3];
    for token in &tags {
        let lower = token.name.to_ascii_lowercase();
        let slot = match lower.as_str() {
            "html" => Some(0),
            "head" => Some(1),
            "body" => Some(2),
            _ => None,
        };
        if let Some(slot) = slot {
            if token.closing {
                closes[slot] += 1;
            } else {
                opens[slot] += 1;
            }
        }
    }
    for (element, slot) in [("html", 0usize), ("head", 1), ("body", 2)] {
        if opens[slot] != 1 || closes[slot] != 1 {
            issues.push(HtmlIssue {
                rule: match element {
                    "html" => "structure.html",
                    "head" => "structure.head",
                    _ => "structure.body",
                },
                at: format!(
                    "{name}: expected exactly one <{element}>…</{element}> pair, found {} open / {} close",
                    opens[slot], closes[slot]
                ),
            });
        }
    }

    // Pass 2: the §3.8 CSP restrictions.
    for token in &tags {
        let lower = token.name.to_ascii_lowercase();
        if token.closing {
            continue;
        }
        // Inline <script> (no src) would be blocked by script-src.
        if lower == "script" && !token.has_attribute("src") {
            issues.push(HtmlIssue {
                rule: "csp.inline_script",
                at: format!("{name}: <script> without src (inline script is forbidden)"),
            });
        }
        // <style> elements are blocked by style-src 'self'.
        if lower == "style" {
            issues.push(HtmlIssue {
                rule: "csp.style_element",
                at: format!("{name}: <style> element (inline CSS is forbidden)"),
            });
        }
        for (attribute_name, attribute_value) in &token.attributes {
            let attr_lower = attribute_name.to_ascii_lowercase();
            let value_lower = attribute_value.trim().to_ascii_lowercase();

            // Inline event handlers.
            if attr_lower.starts_with("on")
                && attr_lower.len() > 2
                && attr_lower.chars().all(|c| c.is_ascii_alphabetic())
            {
                issues.push(HtmlIssue {
                    rule: "csp.event_handler",
                    at: format!("{name}: {attr_lower} attribute (inline handlers are forbidden)"),
                });
            }
            // Style attributes.
            if attr_lower == "style" {
                issues.push(HtmlIssue {
                    rule: "csp.style_attribute",
                    at: format!("{name}: style= attribute (inline CSS is forbidden)"),
                });
            }
            // javascript: URLs.
            if value_lower.trim_start().starts_with("javascript:") {
                issues.push(HtmlIssue {
                    rule: "csp.javascript_url",
                    at: format!(
                        "{name}: {attr_lower}={:?} (javascript: URLs are forbidden)",
                        attribute_value
                    ),
                });
            }
            // External sub-resources: every src/href must be
            // same-origin relative (the package is served at /zim/).
            if (attr_lower == "src" || attr_lower == "href")
                && is_external_reference(attribute_value)
            {
                issues.push(HtmlIssue {
                    rule: "csp.external_reference",
                    at: format!(
                        "{name}: {attr_lower}={:?} must be a package-relative path",
                        attribute_value
                    ),
                });
            }
        }
    }

    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

/// Would this URL reference leave the package's origin?
fn is_external_reference(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("//")
        || lower.starts_with("data:")
        || lower.starts_with("blob:")
        || lower.starts_with("ws://")
        || lower.starts_with("wss://")
        || lower.starts_with("chrome://")
        || lower.starts_with("chrome-untrusted://")
    {
        return true;
    }
    // A path that escapes the package tree.
    value.starts_with("/../") || value == "/.." || value.contains("../")
}

/// One tag token (opening or closing; void elements have no closing
/// token — the structural checks only track the container trio).
#[derive(Debug, Clone)]
struct TagToken {
    name: String,
    closing: bool,
    attributes: Vec<(String, String)>,
}

impl TagToken {
    fn has_attribute(&self, name: &str) -> bool {
        self.attributes
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case(name))
    }
}

/// A deliberately small tokenizer: tags, their names, and
/// attributes. Comments, doctype, and raw text content are skipped;
/// `<script>` bodies (always empty here — inline script is rejected
/// anyway) are not parsed as markup.
fn tokenize(text: &str) -> Vec<TagToken> {
    let bytes = text.as_bytes();
    let mut tags = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'<' {
            index += 1;
            continue;
        }
        // Comments and doctype.
        if text[index..].starts_with("<!--") {
            match text[index + 4..].find("-->") {
                Some(end) => index += 4 + end + 3,
                None => break,
            }
            continue;
        }
        if text[index..].starts_with("<!") || text[index..].starts_with("<?") {
            match text[index..].find('>') {
                Some(end) => index += end + 1,
                None => break,
            }
            continue;
        }
        // Find the tag's end, respecting quoted attribute values.
        let Some((end, raw)) = scan_tag_end(text, index) else {
            break;
        };
        let token = parse_tag(raw);
        if let Some(token) = token {
            tags.push(token);
        }
        index = end;
    }
    tags
}

/// From `<` to the matching `>`: returns (index-after-`>`, raw inner
/// text between the brackets).
fn scan_tag_end(text: &str, start: usize) -> Option<(usize, &str)> {
    let bytes = text.as_bytes();
    let mut index = start + 1;
    let mut quote: Option<u8> = None;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(q) if byte == q => quote = None,
            Some(_) => {}
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None if byte == b'>' => {
                return Some((index + 1, &text[start + 1..index]));
            }
            _ => {}
        }
        index += 1;
    }
    None
}

/// Parse one tag's inner text into a token.
fn parse_tag(raw: &str) -> Option<TagToken> {
    let raw = raw.trim();
    let mut rest = raw;
    let mut closing = false;
    if let Some(stripped) = rest.strip_prefix('/') {
        closing = true;
        rest = stripped;
    } else if rest.ends_with('/') {
        rest = rest[..rest.len() - 1].trim_end();
    }
    // Tag name.
    let name_len = rest
        .find(|c: char| c.is_ascii_whitespace())
        .unwrap_or(rest.len());
    let name = &rest[..name_len];
    if name.is_empty() || !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let mut attributes = Vec::new();
    let mut attribute_text = &rest[name_len..];
    while !attribute_text.is_empty() {
        attribute_text = attribute_text.trim_start();
        if attribute_text.is_empty() {
            break;
        }
        // Attribute name.
        let name_end = attribute_text
            .find(|c: char| c == '=' || c.is_ascii_whitespace())
            .unwrap_or(attribute_text.len());
        let attribute_name = &attribute_text[..name_end];
        attribute_text = &attribute_text[name_end..];
        // Optional =value.
        let mut value = String::new();
        let trimmed = attribute_text.trim_start();
        if let Some(after) = trimmed.strip_prefix('=') {
            let after_trim = after.trim_start();
            if let Some(quote) = after_trim
                .chars()
                .next()
                .filter(|c| *c == '"' || *c == '\'')
            {
                let inner_start = after.len() - after_trim.len() + 1;
                if let Some(end) = after[inner_start..].find(quote) {
                    value = after[inner_start..inner_start + end].to_owned();
                    attribute_text = &after[inner_start + end + 1..];
                } else {
                    attribute_text = "";
                }
            } else {
                let value_end = after_trim
                    .find(char::is_whitespace)
                    .unwrap_or(after_trim.len());
                value = after_trim[..value_end].to_owned();
                let consumed = after.len() - after_trim.len() + value_end;
                attribute_text = &after[consumed..];
            }
        } else {
            attribute_text = trimmed;
        }
        if !attribute_name.is_empty() {
            attributes.push((attribute_name.to_owned(), value));
        }
    }
    Some(TagToken {
        name: name.to_owned(),
        closing,
        attributes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<link rel="stylesheet" href="/zim/assets/main.css">
</head><body><script type="module" src="/zim/assets/app.js"></script>
<a href="/zim/servers">servers</a><img alt="x" src="logo.svg"></body></html>"#;

    #[test]
    fn accepts_a_csp_compliant_document() {
        assert!(check_document("pages/newtab.html", GOOD.as_bytes()).is_ok());
    }

    #[test]
    fn rejects_inline_script() {
        let bad = r#"<html><head></head><body><script>alert(1)</script></body></html>"#;
        let issues = check_document("x.html", bad.as_bytes()).unwrap_err();
        assert!(issues.iter().any(|i| i.rule == "csp.inline_script"));
    }

    #[test]
    fn rejects_style_element_and_attribute() {
        let bad = r#"<html><head><style>p{}</style></head><body><p style="color:red">x</p></body></html>"#;
        let issues = check_document("x.html", bad.as_bytes()).unwrap_err();
        assert!(issues.iter().any(|i| i.rule == "csp.style_element"));
        assert!(issues.iter().any(|i| i.rule == "csp.style_attribute"));
    }

    #[test]
    fn rejects_event_handlers_and_js_urls() {
        let bad = r#"<html><head></head><body onload="go()"><a href="javascript:go()">x</a></body></html>"#;
        let issues = check_document("x.html", bad.as_bytes()).unwrap_err();
        assert!(issues.iter().any(|i| i.rule == "csp.event_handler"));
        assert!(issues.iter().any(|i| i.rule == "csp.javascript_url"));
    }

    #[test]
    fn rejects_external_references() {
        let bad = r#"<html><head><link rel="stylesheet" href="https://cdn.example.com/x.css"></head><body><script src="//evil.example.com/x.js"></script></body></html>"#;
        let issues = check_document("x.html", bad.as_bytes()).unwrap_err();
        assert!(issues.iter().any(|i| i.rule == "csp.external_reference"));
        assert_eq!(
            issues
                .iter()
                .filter(|i| i.rule == "csp.external_reference")
                .count(),
            2
        );
    }

    #[test]
    fn rejects_broken_structure() {
        let bad = "<html><head></head><body></body>"; // no </html>
        let issues = check_document("x.html", bad.as_bytes()).unwrap_err();
        assert!(issues.iter().any(|i| i.rule == "structure.html"));
        let bad = "<html><head></head><body></body></html></html>";
        let issues = check_document("x.html", bad.as_bytes()).unwrap_err();
        assert!(issues.iter().any(|i| i.rule == "structure.html"));
        let bad = "";
        let issues = check_document("x.html", bad.as_bytes()).unwrap_err();
        assert!(issues.iter().any(|i| i.rule == "structure.html"));
    }

    #[test]
    fn attributes_with_quoted_values_parse() {
        // The tokenizer must not trip on '>' inside quoted values.
        let doc = r#"<html><head></head><body data-note="a > b"><a href="/zim/x" title="c > d">x</a></body></html>"#;
        assert!(check_document("x.html", doc.as_bytes()).is_ok());
    }
}
