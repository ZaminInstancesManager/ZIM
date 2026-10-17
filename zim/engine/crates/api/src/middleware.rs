//! The middleware stack, outermost first (per `routes.rs`):
//!
//! 1. `host_guard` — DNS-rebinding mitigation: only loopback hosts.
//! 2. `request_id` — correlation ids on every response; injected
//!    into error bodies.
//! 3. `version` — `X-ZIM-API-Version` advertising and `Accept`
//!    version negotiation (406 on an unsupported requested version).
//! 4. `addon_attribution` — per-addon call attribution (AD-29's
//!    M27 hardening): reads the vendored SDK's `X-ZIM-Addon` header,
//!    validates it against the installed+enabled registry, and
//!    publishes it (plus the user agent) to the audit writer through
//!    a task-local.
//! 5. `authenticate` — cookie/bearer credential resolution into the
//!    `Caller` extension.
//! 6. `credential_rate_limit` — per-credential sliding window (§9).
//! 7. `idempotency_replay` — `Idempotency-Key` response replay (§12).
//!
//! The body-size limit is applied by the router itself (axum's
//! `DefaultBodyLimit`), not a middleware.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::Utc;

use crate::error::ApiError;
use crate::state::{AppState, Caller, CredentialKind};

/// Header carrying the per-request correlation id.
pub const REQUEST_ID_HEADER: &str = "x-zim-request-id";

/// The addon-attribution header the vendored SDK's fetch wrapper
/// stamps on every API call an addon page makes (AD-29: advisory
/// attribution on the shared loopback origin — authorization is
/// unchanged, the user's own credentials still gate every call).
pub const ADDON_HEADER: &str = "x-zim-addon";

/// The longest addon id accepted for attribution (reverse-DNS ids
/// are far shorter; the cap only guards pathological headers).
const MAX_ADDON_ID_LEN: usize = 255;

/// The per-request attribution the audit writer reads (set by
/// `addon_attribution`, consumed by `handlers::audit`).
#[derive(Debug, Clone, Default)]
pub struct CallAttribution {
    /// The addon the calling page belongs to, when the header named
    /// an installed AND enabled one.
    pub addon_id: Option<String>,
    /// The request's `User-Agent`, when presented.
    pub user_agent: Option<String>,
}

tokio::task_local! {
    static CALL_ATTRIBUTION: CallAttribution;
}

/// Read the current request's attribution (outside a request scope:
/// the empty attribution — the audit writer's honest default).
pub fn current_attribution() -> CallAttribution {
    CALL_ATTRIBUTION
        .try_with(|slot| slot.clone())
        .unwrap_or_default()
}

/// Maximum size of an error body that the request-id middleware will
/// buffer for injection (error bodies are small JSON; the cap only
/// guards pathological cases).
const ERROR_BODY_INJECT_LIMIT: usize = 64 * 1024;

// ── 1. Host validation (DNS-rebinding mitigation) ─────────────────

/// Is this a `Host` value the loopback engine accepts?
///
/// Accepted: `127.0.0.1[:port]`, `localhost[:port]`, `[::1][:port]`,
/// and the bare `::1`. Everything else — including any
/// attacker-controlled DNS name that happens to resolve to loopback
/// — is rejected with `400 bad_request` before routing.
pub fn is_loopback_host(host: &str) -> bool {
    let host = host.trim();
    if host.is_empty() {
        return false;
    }
    // Bracketed IPv6 form.
    if let Some(rest) = host.strip_prefix('[') {
        let addr = rest.split(']').next().unwrap_or("");
        return addr.eq_ignore_ascii_case("::1");
    }
    // Strip an optional trailing :port. A host with more than one
    // colon is a bare IPv6 literal (bracketed forms were handled
    // above; bare IPv6 has no port form in HTTP/1.1).
    let bare = if host.matches(':').count() > 1 {
        host
    } else {
        host.rsplit_once(':').map_or(host, |(h, p)| {
            if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) && !h.is_empty() {
                h
            } else {
                host
            }
        })
    };
    bare.eq_ignore_ascii_case("localhost")
        || bare == "127.0.0.1"
        || bare.eq_ignore_ascii_case("::1")
}

/// Middleware: reject non-loopback `Host` headers (§1 loopback-only
/// transport, DNS-rebinding mitigation).
pub async fn host_guard(req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    match host.as_deref() {
        Some(h) if is_loopback_host(h) => next.run(req).await,
        Some(h) => ApiError::bad_request(format!(
            "Host {h:?} is not a loopback host; this engine only serves 127.0.0.1, localhost \
             and ::1 (DNS-rebinding protection)"
        ))
        .into_response(),
        None => ApiError::bad_request("requests must carry a Host header").into_response(),
    }
}

// ── 1b. Addon attribution (AD-29's M27 hardening) ─────────────────

/// Middleware: publish the request's call attribution to the audit
/// writer.
///
/// The vendored SDK stamps `X-ZIM-Addon: <addon-id>` on every API
/// call an addon page makes. The header is **advisory by design**:
/// every page on the engine's origin can set any header it likes,
/// so this can never be an enforcement boundary — which is exactly
/// why AD-29 records it as *attribution*, not authorization. What
/// the engine contributes is the honest half: the id is only
/// believed when it names an addon that is **installed and enabled**
/// (a spoofed or stale id is dropped, never recorded as fact), and
/// when believed it rides the audit record as `metadata.addon_id`
/// next to the *user* whose credentials actually authorized the
/// call.
///
/// The user agent rides the same task-local so audit records stop
/// carrying `user_agent: null` for API-originated actions (a small
/// honesty gap noticed while wiring this).
///
/// Attribution is never a request modifier: the request reaches the
/// handler byte-identical whether the header was believed, dropped,
/// or absent — including for host/version/authz rejections, which
/// this middleware deliberately sits outside of.
pub async fn addon_attribution(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let attribution = CallAttribution {
        addon_id: presented_addon_id(&state, req.headers()),
        user_agent: req
            .headers()
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
    };
    CALL_ATTRIBUTION.scope(attribution, next.run(req)).await
}

/// Resolve the believed addon id from the headers: presented, short
/// enough, and matching an installed + enabled addon. Anything else
/// is `None` — unknown ids, disabled addons, and uninstalled addons
/// are all "not attributable", not errors (a page whose addon was
/// disabled mid-session keeps working under its user's credentials
/// with plain user attribution, which is precisely AD-29's
/// disable-with-notice posture).
fn presented_addon_id(state: &AppState, headers: &axum::http::HeaderMap) -> Option<String> {
    let raw = headers
        .get(ADDON_HEADER)
        .and_then(|v| v.to_str().ok())?
        .trim()
        .to_owned();
    if raw.is_empty() || raw.len() > MAX_ADDON_ID_LEN {
        return None;
    }
    let installed_and_enabled = state
        .extensions
        .list()
        .iter()
        .any(|addon| addon.enabled && addon.id == raw);
    installed_and_enabled.then_some(raw)
}

// ── 2. Request id ─────────────────────────────────────────────────

/// Middleware: generate `req_<uuid>` correlation ids; stamp the
/// response header and inject the id into any structured error body.
pub async fn request_id(req: Request, next: Next) -> Response {
    let request_id = format!("req_{}", uuid::Uuid::new_v4().simple());
    let response = next.run(req).await;
    inject_request_id(response, &request_id).await
}

/// Stamp the header and rewrite error bodies with the request id.
async fn inject_request_id(mut response: Response, request_id: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(request_id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    let status = response.status();
    if !(status.is_client_error() || status.is_server_error()) {
        return response;
    }
    // Buffer the (small) error body and add the request id to the
    // `error` object, preserving every other field.
    let (mut parts, body) = response.into_parts();
    let buffered = match axum::body::to_bytes(body, ERROR_BODY_INJECT_LIMIT).await {
        Ok(bytes) => bytes,
        Err(_) => return Response::from_parts(parts, Body::empty()),
    };
    match serde_json::from_slice::<serde_json::Value>(&buffered) {
        Ok(mut value) => {
            if let Some(error) = value.get_mut("error").and_then(|e| e.as_object_mut()) {
                error.insert("request_id".to_owned(), serde_json::json!(request_id));
            }
            let new_body = serde_json::to_vec(&value).unwrap_or_default();
            parts
                .headers
                .insert(header::CONTENT_LENGTH, new_body.len().into());
            Response::from_parts(parts, Body::from(new_body))
        }
        // Not our structured shape: pass the original body through.
        Err(_) => Response::from_parts(parts, Body::from(buffered.to_vec())),
    }
}

// ── 3. API version ────────────────────────────────────────────────

/// Middleware: advertise `X-ZIM-API-Version`; reject unsupported
/// `Accept: ...; version=vN` requests with 406 + the supported list.
pub async fn version(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let supported = state.config.api_version;
    // Loose Accept parse: only look at explicit `version=` params.
    if let Some(accept) = req
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
    {
        let requested = accept
            .split(',')
            .filter_map(|part| part.split(';').nth(1))
            .filter_map(|param| param.trim().strip_prefix("version="))
            .map(str::to_owned)
            .next();
        if let Some(requested) = requested {
            let requested_major = requested
                .trim_start_matches('v')
                .split('.')
                .next()
                .unwrap_or("");
            let supported_major = supported
                .trim_start_matches('v')
                .split('.')
                .next()
                .unwrap_or("");
            if requested_major != supported_major {
                let error = ApiError::bad_request(format!(
                    "API version {requested:?} is not supported; this engine speaks \
                     {supported} (supported major versions: v1)"
                ));
                let mut response = error.into_response();
                // The requested version's failure is 406 per spec §2,
                // carrying the supported versions (documented mapping:
                // code `bad_request` with HTTP 406).
                *response.status_mut() = StatusCode::NOT_ACCEPTABLE;
                if let Ok(v) = HeaderValue::from_str(supported) {
                    response.headers_mut().insert("x-zim-api-version", v);
                }
                return response;
            }
        }
    }
    let mut response = next.run(req).await;
    if let Ok(value) = HeaderValue::from_str(supported) {
        response.headers_mut().insert("x-zim-api-version", value);
    }
    response
}

// ── 4. Authentication ─────────────────────────────────────────────

/// Extract the session cookie value for `name` from a `Cookie` header
/// set (multiple `Cookie` headers, `;`-separated pairs).
fn cookie_value(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    for header_value in headers.get_all(header::COOKIE) {
        let raw = header_value.to_str().ok()?;
        for pair in raw.split(';') {
            let pair = pair.trim();
            if let Some(value) = pair.strip_prefix(&prefix) {
                return Some(value.to_owned());
            }
        }
    }
    None
}

/// Middleware: resolve the caller from a bearer token or session
/// cookie and stash it in the request extensions.
///
/// Presented-but-invalid credentials produce a `401` immediately —
/// except on the login/health paths, where a stale cookie (a natural
/// browser state) must not block re-authentication.
pub async fn authenticate(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let now = Utc::now();
    // Note: `Request<Body>` is Send but not Sync, so nothing may
    // borrow it across an await — extract the owned pieces up front
    // (path, headers) and move the request on untouched.
    let path = req.uri().path().to_owned();
    let headers = req.headers().clone();
    let credentials_optional = path.ends_with("/auth/login") || path.ends_with("/health");

    let caller = match resolve_caller(&state, &headers, &path, now).await {
        Ok(caller) => caller,
        Err(error) => {
            if credentials_optional {
                Caller::anonymous()
            } else {
                return error.into_response();
            }
        }
    };

    let mut req = req;
    req.extensions_mut().insert(caller);
    next.run(req).await
}

/// Credential resolution: bearer token first, then cookie.
///
/// Takes the request's headers and path as owned/sync data (see the
/// note in [`authenticate`]).
async fn resolve_caller(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    path: &str,
    now: chrono::DateTime<Utc>,
) -> Result<Caller, ApiError> {
    let _ = path;
    if let Some(bearer) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
    {
        let record = state
            .api_keys
            .validate(bearer, now)
            .map_err(|e| ApiError::unauthorized(format!("invalid API key credential: {e}")))?;
        return finish_user(
            state,
            record.user_id,
            Some(CredentialKind::ApiKey),
            format!("key:{}", record.key_id),
        )
        .await;
    }
    if let Some(token) = cookie_value(headers, state.config.session_cookie) {
        let session = state
            .sessions
            .validate(&token, now)
            .map_err(|e| ApiError::unauthorized(format!("invalid session credential: {e}")))?;
        return finish_user(
            state,
            session.user_id,
            Some(CredentialKind::Session),
            format!("sess:{}", session.token_hash),
        )
        .await;
    }
    Ok(Caller::anonymous())
}

/// Load the credential's user row (spawn_blocking over the sync port).
async fn finish_user(
    state: &AppState,
    user_id: zim_engine_domain::UserId,
    kind: Option<CredentialKind>,
    rate_key: String,
) -> Result<Caller, ApiError> {
    let persistence = Arc::clone(&state.persistence);
    let user = tokio::task::spawn_blocking(move || persistence.get_user(&user_id))
        .await
        .map_err(|e| ApiError::internal(format!("persistence task join error: {e}")))?
        .map_err(|e| ApiError::internal(format!("persistence failure: {e}")))?;
    match user {
        Some(user) => Ok(Caller {
            user: Some(user),
            credential: kind,
            rate_key: Some(rate_key),
        }),
        None => Err(ApiError::unauthorized(
            "credential names a user that no longer exists",
        )),
    }
}

// ── 5. Per-credential rate limiting ───────────────────────────────

/// Middleware: per-credential sliding-window limiter (§9).
///
/// Anonymous requests are not limited here (login has its own per-IP
/// limiter; everything else anonymous is already a 401).
pub async fn credential_rate_limit(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let key = req
        .extensions()
        .get::<Caller>()
        .and_then(|c| c.rate_key.clone());
    if let Some(key) = key {
        let now = Utc::now();
        let decision = state.credential_limiter.check(&key, now);
        if decision.allowed {
            // Amortized prune so churn does not grow the map forever.
            if state.credential_limiter.key_count() % 1009 == 0 {
                state.credential_limiter.prune(now);
            }
            next.run(req).await
        } else {
            let retry_after = (decision.resets_at - now).num_seconds().clamp(1, 3600) as u64;
            let limit = state.credential_limiter.limit();
            let mut response = ApiError::RateLimited {
                retry_after_seconds: retry_after,
                limit,
            }
            .into_response();
            apply_rate_headers(&mut response, limit, 0, decision.resets_at, retry_after);
            response
        }
    } else {
        next.run(req).await
    }
}

/// Stamp the `X-ZIM-RateLimit-*` trio (§9) on a 429 response.
fn apply_rate_headers(
    response: &mut Response,
    limit: u64,
    remaining: u64,
    reset: chrono::DateTime<Utc>,
    retry_after: u64,
) {
    let headers = response.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&limit.to_string()) {
        headers.insert("x-zim-ratelimit-limit", v);
    }
    if let Ok(v) = HeaderValue::from_str(&remaining.to_string()) {
        headers.insert("x-zim-ratelimit-remaining", v);
    }
    if let Ok(v) = HeaderValue::from_str(&reset.timestamp().to_string()) {
        headers.insert("x-zim-ratelimit-reset", v);
    }
    if let Ok(v) = HeaderValue::from_str(&retry_after.to_string()) {
        headers.insert(header::RETRY_AFTER, v);
    }
}

// ── 6. Idempotency replay ─────────────────────────────────────────

/// Header set on replayed responses.
pub const IDEMPOTENT_REPLAY_HEADER: &str = "x-zim-idempotent-replay";

/// The longest `Idempotency-Key` accepted (guards unbounded keys).
const MAX_IDEMPOTENCY_KEY_LEN: usize = 255;

/// Middleware: implement §12 for mutating methods.
///
/// Fingerprint = `"<METHOD> <path?query>"` (namespace-scoped by the
/// caller). See the module docs for the deliberate scope choices.
pub async fn idempotency_replay(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let method = req.method().clone();
    if !matches!(
        method,
        axum::http::Method::POST
            | axum::http::Method::PATCH
            | axum::http::Method::PUT
            | axum::http::Method::DELETE
    ) {
        return next.run(req).await;
    }
    let key = req
        .headers()
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let Some(key) = key else {
        return next.run(req).await;
    };
    if key.is_empty() || key.len() > MAX_IDEMPOTENCY_KEY_LEN {
        return ApiError::validation(
            format!("Idempotency-Key must be 1..={MAX_IDEMPOTENCY_KEY_LEN} characters"),
            "Idempotency-Key",
        )
        .into_response();
    }
    let namespace = req
        .extensions()
        .get::<Caller>()
        .map(Caller::idempotency_namespace)
        .unwrap_or_else(|| "anon".to_owned());

    // Buffer the request body to fingerprint the operation
    // (method + path + query + body hash): reusing a key for a
    // different payload is an error, not a replay. The body is
    // re-created from the buffered bytes so the handler sees it
    // unchanged.
    let (parts, body) = req.into_parts();
    let buffered = match axum::body::to_bytes(body, state.config.max_body_bytes).await {
        Ok(bytes) => bytes,
        Err(e) => {
            return ApiError::bad_request(format!("could not buffer the request body: {e}"))
                .into_response();
        }
    };
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(&buffered);
    let fingerprint = format!("{} {} sha256:{digest:x}", method, parts.uri);
    let req = Request::from_parts(parts, Body::from(buffered.to_vec()));

    let now = Utc::now();
    if let Some(stored) = state.idempotency.lookup(&namespace, &key, now) {
        if stored.fingerprint != fingerprint {
            return ApiError::bad_request(
                "Idempotency-Key was already used for a different request; \
                 keys are single-use per operation"
                    .to_string(),
            )
            .into_response();
        }
        let mut response = Response::builder()
            .status(StatusCode::from_u16(stored.status).unwrap_or(StatusCode::OK))
            .body(Body::from(stored.body.clone()))
            .expect("static response parts");
        if let Some(content_type) = &stored.content_type {
            if let Ok(v) = HeaderValue::from_str(content_type) {
                response.headers_mut().insert(header::CONTENT_TYPE, v);
            }
        }
        response
            .headers_mut()
            .insert(IDEMPOTENT_REPLAY_HEADER, HeaderValue::from_static("true"));
        return response;
    }

    let response = next.run(req).await;
    store_response(&state, &namespace, &key, fingerprint, response, now).await
}

/// Buffer a completed response into the idempotency store when it is
/// worth replaying (any status except 429 and 5xx).
async fn store_response(
    state: &AppState,
    namespace: &str,
    key: &str,
    fingerprint: String,
    response: Response,
    now: chrono::DateTime<Utc>,
) -> Response {
    let status = response.status();
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, state.config.max_body_bytes)
        .await
        .unwrap_or_default();
    let rebuilt = Response::from_parts(parts, Body::from(bytes.to_vec()));
    if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        return rebuilt;
    }
    let content_type = rebuilt
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    state.idempotency.store(
        namespace,
        key,
        fingerprint,
        crate::idempotency::StoredResponse {
            status: status.as_u16(),
            body: bytes.to_vec(),
            content_type,
            expires_at: now,
            // The caller-supplied fingerprint (the third argument)
            // overrides this placeholder inside `store`.
            fingerprint: String::new(),
        },
        now,
    );
    rebuilt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_host_whitelist() {
        for good in [
            "127.0.0.1",
            "127.0.0.1:8080",
            "localhost",
            "localhost:1",
            "LocalHost:443",
            "[::1]",
            "[::1]:8443",
            "::1",
        ] {
            assert!(is_loopback_host(good), "{good} should be allowed");
        }
        for bad in [
            "evil.example.com",
            "evil.example.com:443",
            "10.0.0.1",
            "169.254.169.254",
            "",
            "127.0.0.2",
            "[2001:db8::1]",
            "127.0.0.1.evil.com",
        ] {
            assert!(!is_loopback_host(bad), "{bad} must be rejected");
        }
    }
}
