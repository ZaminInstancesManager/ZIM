//! The `/zim/` page surface (M11): serving the active frontend
//! package's pages and assets over the engine's loopback HTTPS
//! origin.
//!
//! This is the engine half of the `zim://` URL inventory
//! (BROWSER_INTEGRATION.md §3): the desktop shell's webview loads
//! these pages through the AD-23 loopback-origin mapping, and the
//! same content is directly reachable at
//! `https://127.0.0.1:{port}/zim/…` so the default frontend is
//! usable in any browser today.
//!
//! # Security posture
//!
//! - Page serving is unauthenticated by design: the pages are static
//!   shells with no secrets; all data flows through the
//!   credential-checked `/api/v1` endpoints (capability checks stay
//!   at the engine boundary — architecture invariant 15).
//! - Every response carries the §3.8 CSP (`default-src 'none'`,
//!   scripts/styles/connect only from the same origin, no inline
//!   anything), `X-Content-Type-Options: nosniff`, and
//!   `Referrer-Policy: no-referrer`.
//! - M22 framing posture (AD-24): the app chrome (`zim://app`) hosts
//!   tab content in same-origin iframes, so the CSP additionally
//!   allows `frame-src 'self'` (a page may embed same-origin panes)
//!   and `frame-ancestors 'self'` (a page may be embedded by the
//!   chrome) — `X-Frame-Options: SAMEORIGIN` matches. Cross-origin
//!   embedding stays blocked, and the loopback host guard (below)
//!   already confines who can even talk to this origin.
//! - Paths are routed through the loader's confinement checks; a
//!   traversal attempt answers 404, never a filesystem read.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::state::AppState;
use zim_engine_frontend_loader::Resolution;

/// The §3.8 content security policy every `/zim/` response carries.
///
/// `frame-src`/`frame-ancestors 'self'` (M22, AD-24): the app chrome
/// embeds tab panes as same-origin iframes — same-origin only, in
/// both directions, so a foreign page can neither embed ZIM surfaces
/// nor be embedded by them.
pub const ZIM_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
     connect-src 'self'; img-src 'self' data:; font-src 'self'; base-uri 'none'; \
     form-action 'none'; frame-src 'self'; frame-ancestors 'self'";

/// `GET /zim` and `GET /zim/` — the package root routes to the
/// `newtab` entrypoint (the NTP-replacement rule, §8).
pub async fn root(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    serve_path(&state, "", &headers).await
}

/// `GET /zim/{*path}` — one page or asset from the active package.
///
/// Deliberately unauthenticated (static shells; data flows through
/// the credential-checked API) — hence no `Caller` extraction here.
pub async fn serve(
    State(state): State<Arc<AppState>>,
    Path(path): Path<String>,
    headers: HeaderMap,
) -> Response {
    serve_path(&state, &path, &headers).await
}

/// Resolve + serve one package path, honoring `If-None-Match`
/// revalidation against the content digest ETag.
async fn serve_path(state: &Arc<AppState>, path: &str, headers: &HeaderMap) -> Response {
    let frontend = Arc::clone(&state.frontend);
    let owned = path.to_owned();
    let resolution = frontend.resolve(&owned);
    match resolution {
        Resolution::File {
            path: file,
            mime,
            cacheable,
        } => {
            let Some(bytes) = frontend.file(&file) else {
                return not_found_page();
            };
            let digest = frontend.file_digest(&file);
            let etag = digest.as_ref().map(|digest| format!("\"{digest}\""));
            // §12-style revalidation: a matching If-None-Match (weak
            // comparison, `W/` prefixes tolerated) short-circuits.
            if let (Some(etag), Some(if_none_match)) = (
                etag.as_deref(),
                headers
                    .get(header::IF_NONE_MATCH)
                    .and_then(|v| v.to_str().ok()),
            ) {
                if if_none_match
                    .split(',')
                    .map(|candidate| candidate.trim().trim_start_matches("W/"))
                    .any(|candidate| candidate == etag || candidate == "*")
                {
                    let mut response = StatusCode::NOT_MODIFIED.into_response();
                    if let Ok(value) = HeaderValue::from_str(etag) {
                        response.headers_mut().insert(header::ETAG, value);
                    }
                    return response;
                }
            }
            let mut headers = HeaderMap::new();
            security_headers(&mut headers);
            if let Ok(value) = HeaderValue::from_str(mime) {
                headers.insert(header::CONTENT_TYPE, value);
            }
            if cacheable {
                headers.insert(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=3600"),
                );
            } else {
                headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            }
            // Serving-side ETag from the content digest.
            if let Some(etag) = etag {
                if let Ok(value) = HeaderValue::from_str(&etag) {
                    headers.insert(header::ETAG, value);
                }
            }
            (StatusCode::OK, headers, bytes).into_response()
        }
        Resolution::NotFound => not_found_page(),
    }
}

/// The engine's built-in 404 page for undeclared surfaces (§3.2):
/// a minimal, CSP-clean document — the loader cannot serve one from
/// the package (the surface is not declared), so the engine itself
/// answers.
fn not_found_page() -> Response {
    const BODY: &str = r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>ZIM - not found</title>
<link rel="stylesheet" href="/zim/assets/main.css">
</head><body><main class="page">
<h1>404 - surface not declared</h1>
<p class="empty">The active frontend package does not declare this zim:// surface.</p>
<p><a href="/zim/newtab">Back to the new-tab page</a></p>
</main></body></html>"#;
    let mut headers = HeaderMap::new();
    security_headers(&mut headers);
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    (StatusCode::NOT_FOUND, headers, BODY.as_bytes().to_vec()).into_response()
}

/// The security header set shared by every `/zim/` response.
fn security_headers(headers: &mut HeaderMap) {
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(ZIM_CSP),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    // M22 (AD-24): the chrome's same-origin iframes require
    // SAMEORIGIN framing; cross-origin embedding stays denied (the
    // CSP's `frame-ancestors 'self'` says the same thing, and both
    // headers are enforced).
    headers.insert("x-frame-options", HeaderValue::from_static("SAMEORIGIN"));
}
