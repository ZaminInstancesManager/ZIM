//! `/api/v1/auth/*` — login, logout, refresh, and the OAuth stubs.
//!
//! Login verifies the Argon2id hash (CPU-bound: wrapped in
//! `spawn_blocking`), issues a session via the M4 session manager,
//! and sets the cookie with the spec-mandated attributes
//! (`HttpOnly`, `Secure`, `SameSite=Strict`, `Path=/api/v1`, spec
//! §3). Refresh rotates the session token (issue + revoke) so a
//! stolen token stops working the moment the legitimate client
//! refreshes.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::Utc;
use serde::Deserialize;

use crate::error::ApiError;
use crate::handlers::{audit, require_user, user_json, ApiJson};
use crate::state::{AppState, Caller};

/// `POST /api/v1/auth/login`.
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    /// Login email.
    pub email: String,
    /// Account password (Argon2id-verified).
    pub password: String,
}

/// The per-IP login limiter's bucket key.
fn login_bucket_key(addr: &SocketAddr) -> String {
    // The engine is loopback-only, so per-IP buckets are per-remote
    // (all loopback in practice; the key shape keeps the policy
    // honest if a UDS/forwarded variant ever appears).
    format!("ip:{}", addr.ip())
}

/// Set the session cookie with the spec §3 attributes.
fn set_session_cookie(response: &mut Response, state: &AppState, token: &str) {
    let cookie = format!(
        "{}={}; HttpOnly; Secure; SameSite=Strict; Path={}",
        state.config.session_cookie,
        token,
        state.config.session_cookie_path(),
    );
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
}

/// Expire the session cookie (logout).
fn clear_session_cookie(response: &mut Response, state: &AppState) {
    let cookie = format!(
        "{}=; HttpOnly; Secure; SameSite=Strict; Path={}; Max-Age=0",
        state.config.session_cookie,
        state.config.session_cookie_path(),
    );
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
}

/// Extract the raw session token from a request's cookie header.
fn raw_session_token(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    for value in headers.get_all(header::COOKIE) {
        let raw = value.to_str().ok()?;
        for pair in raw.split(';') {
            if let Some(token) = pair.trim().strip_prefix(&prefix) {
                return Some(token.to_owned());
            }
        }
    }
    None
}

/// `POST /api/v1/auth/login` — email + password → session cookie.
pub async fn login(
    State(state): State<Arc<AppState>>,
    Extension(addr): Extension<SocketAddr>,
    ApiJson(request): ApiJson<LoginRequest>,
) -> Result<Response, ApiError> {
    // §9 brute-force protection: 10 attempts / minute / IP.
    let now = Utc::now();
    let decision = state.login_limiter.check(&login_bucket_key(&addr), now);
    if !decision.allowed {
        let retry_after = (decision.resets_at - now).num_seconds().clamp(1, 3600) as u64;
        return Err(ApiError::RateLimited {
            retry_after_seconds: retry_after,
            limit: state.login_limiter.limit(),
        });
    }

    let email = request.email.trim().to_lowercase();
    let user = {
        let email = email.clone();
        crate::state::db_or_500(&state, move |p| p.get_user_by_email(&email)).await?
    };
    // Uniform failure: unknown email and wrong password are the same
    // answer (no account enumeration).
    let failure = || ApiError::unauthorized("invalid email or password");
    let user = user.ok_or_else(failure)?;
    let hash = user.password_hash.clone().ok_or_else(failure)?;

    // Argon2id verification is deliberately CPU-heavy; keep it off
    // the async workers.
    let password = request.password;
    let verified = tokio::task::spawn_blocking(move || {
        zim_engine_auth::password::verify_password(&password, &hash)
    })
    .await
    .map_err(|e| ApiError::internal(format!("password verify task failed: {e}")))?
    .map_err(|e| ApiError::internal(format!("password verify failed: {e}")))?;
    if !verified {
        return Err(failure());
    }

    let issued = state.sessions.issue(&user.id, Utc::now());
    audit(
        &state,
        Some(user.id.to_string()),
        "auth.login",
        None,
        serde_json::json!({ "email": email }),
    )
    .await;

    let mut response = user_json(&user).into_response();
    set_session_cookie(&mut response, &state, &issued.token);
    Ok(response)
}

/// `POST /api/v1/auth/logout` — revoke the presented session.
pub async fn logout(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    caller: Extension<Caller>,
) -> Result<Response, ApiError> {
    // Revoke by the presented token (not "all sessions"): the cookie
    // in this request is the one going away.
    if let Some(token) = raw_session_token(&headers, state.config.session_cookie) {
        let _ = state.sessions.revoke(&token, Utc::now());
    }
    if let Ok(user) = require_user(&caller) {
        audit(
            &state,
            Some(user.id.to_string()),
            "auth.logout",
            None,
            serde_json::json!({}),
        )
        .await;
    }
    let mut response = Json(serde_json::json!({ "data": null })).into_response();
    clear_session_cookie(&mut response, &state);
    Ok(response)
}

/// `POST /api/v1/auth/refresh` — rotate the session token.
pub async fn refresh(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    caller: Extension<Caller>,
) -> Result<Response, ApiError> {
    let user = require_user(&caller)?;
    let token = raw_session_token(&headers, state.config.session_cookie)
        .ok_or_else(|| ApiError::unauthorized("refresh requires the session cookie credential"))?;
    // Rotation: validate (slides idle), issue the successor, revoke
    // the predecessor. A stolen token dies at the legitimate
    // client's next refresh.
    state
        .sessions
        .validate(&token, Utc::now())
        .map_err(|e| ApiError::unauthorized(format!("invalid session credential: {e}")))?;
    let issued = state.sessions.issue(&user.id, Utc::now());
    let _ = state.sessions.revoke(&token, Utc::now());
    let mut response = user_json(user).into_response();
    set_session_cookie(&mut response, &state, &issued.token);
    Ok(response)
}

/// `GET /api/v1/auth/oauth/{provider}/start` — 501 (Discord-first
/// OAuth lands after M5 per the roadmap's M4 note; the provider set
/// itself is an open question per spec §15 Q2).
pub async fn oauth_start(Path(_provider): Path<String>) -> Result<Response, ApiError> {
    Err(ApiError::NotImplemented {
        feature: "OAuth login flow (Discord-first)",
        milestone: "M5+ (post-API-surface)",
    })
}

/// `GET /api/v1/auth/oauth/{provider}/callback` — 501.
pub async fn oauth_callback(Path(_provider): Path<String>) -> Result<Response, ApiError> {
    Err(ApiError::NotImplemented {
        feature: "OAuth callback handling (Discord-first)",
        milestone: "M5+ (post-API-surface)",
    })
}
