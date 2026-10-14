//! `/api/v1/frontend/*` — frontend-package management (§10.14, M11).
//!
//! - `GET  /frontend/active` — the active package's info (any
//!   session).
//! - `POST /frontend/active` — activate a stored package or the
//!   default (admin). Runs the §3.5 gate; a failing package answers
//!   `422 frontend_smoke_test_failed` with the check-by-check
//!   report, and the previously active package is kept.
//! - `POST /frontend/upload` — upload a ZIP package (admin, raw
//!   `application/zip` body). Stores it and returns the summary plus
//!   its gate report, without activating.
//! - `POST /frontend/smoke-test` — run the gate on a stored package
//!   without activating (admin).
//! - `POST /frontend/reset` — revert to the bundled default (admin).
//!
//! Uploads are sync file work (small archives, bounded by the
//! loader's package limits) and are bridged with `spawn_blocking`
//! like every other blocking hop in this crate.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::json;

use crate::error::ApiError;
use crate::handlers::{audit, require_user};
use crate::state::{AppState, Caller};
use zim_engine_frontend_loader::{FrontendError, SmokeReport};

/// `GET /api/v1/frontend/active` — active package info (any session).
pub async fn active(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_user(&caller)?;
    let info = state.frontend.active_info();
    Ok(Json(serde_json::to_value(&info).unwrap_or_default()))
}

/// `POST /api/v1/frontend/active` request body.
#[derive(Debug, Deserialize)]
pub struct ActivateRequest {
    /// The package id (`"default"` or the bundled default's id resets),
    /// or an uploaded package's id.
    pub id: String,
    /// Optional explicit version (defaults to the highest stored).
    pub version: Option<String>,
    /// Explicit admin approval for unsigned / unknown-author
    /// packages (§3.4).
    #[serde(default)]
    pub approve_untrusted: bool,
}

/// `POST /api/v1/frontend/active` — activate a package (admin).
pub async fn set_active(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user = require_user(&caller)?;
    if !caller.is_admin() {
        return Err(ApiError::AdminRequired(user.id));
    }
    let Json(value) = body.ok_or_else(|| {
        ApiError::validation(
            "the request body must be a JSON object with an \"id\" field",
            "id",
        )
    })?;
    let request: ActivateRequest = serde_json::from_value(value)
        .map_err(|e| ApiError::validation(format!("invalid activation request: {e}"), "id"))?;

    let frontend = Arc::clone(&state.frontend);
    let id = request.id.clone();
    let version = request.version.clone();
    let approve = request.approve_untrusted;
    let outcome =
        tokio::task::spawn_blocking(move || frontend.activate(&id, version.as_deref(), approve))
            .await
            .map_err(|e| ApiError::internal(format!("frontend activation task failed: {e}")))?;

    match outcome {
        Ok(info) => {
            audit(
                &state,
                Some(user.id.to_string()),
                "frontend.activate",
                None,
                json!({ "id": info.id, "version": info.version, "is_default": info.is_default }),
            )
            .await;
            Ok(Json(serde_json::to_value(&info).unwrap_or_default()))
        }
        Err(error) => Err(map_error(error)),
    }
}

/// `POST /api/v1/frontend/upload` — store a package from ZIP bytes
/// (admin; `Content-Type: application/zip`).
pub async fn upload(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let user = require_user(&caller)?;
    if !caller.is_admin() {
        return Err(ApiError::AdminRequired(user.id));
    }
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with("application/zip")
        && !content_type.starts_with("application/octet-stream")
    {
        return Err(ApiError::UnsupportedMediaType(
            "the upload body must be a ZIP archive (Content-Type: application/zip)".to_owned(),
        ));
    }
    if body.is_empty() {
        return Err(ApiError::bad_request("the upload body is empty"));
    }

    let frontend = Arc::clone(&state.frontend);
    let bytes = body.to_vec();
    let outcome = tokio::task::spawn_blocking(move || frontend.upload(&bytes))
        .await
        .map_err(|e| ApiError::internal(format!("frontend upload task failed: {e}")))?;

    match outcome {
        Ok((summary, report)) => {
            audit(
                &state,
                Some(user.id.to_string()),
                "frontend.upload",
                None,
                json!({
                    "id": summary.id,
                    "version": summary.version,
                    "signature": summary.signature,
                    "bytes": summary.bytes,
                    "smoke_passed": report.passed(),
                }),
            )
            .await;
            let body = json!({
                "object": "frontend_package",
                "package": summary,
                "smoke": smoke_json(&report),
            });
            Ok((axum::http::StatusCode::CREATED, Json(body)).into_response())
        }
        Err(error) => Err(map_error(error)),
    }
}

/// `POST /api/v1/frontend/smoke-test` request body.
#[derive(Debug, Deserialize)]
pub struct SmokeTestRequest {
    /// The stored package id to gate.
    pub id: String,
    /// Optional explicit version.
    pub version: Option<String>,
}

/// `POST /api/v1/frontend/smoke-test` — gate a stored package
/// without activating (admin).
pub async fn smoke_test(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user = require_user(&caller)?;
    if !caller.is_admin() {
        return Err(ApiError::AdminRequired(user.id));
    }
    let Json(value) = body.ok_or_else(|| {
        ApiError::validation(
            "the request body must be a JSON object with an \"id\" field",
            "id",
        )
    })?;
    let request: SmokeTestRequest = serde_json::from_value(value)
        .map_err(|e| ApiError::validation(format!("invalid smoke-test request: {e}"), "id"))?;

    let frontend = Arc::clone(&state.frontend);
    let id = request.id.clone();
    let version = request.version.clone();
    let report = tokio::task::spawn_blocking(move || frontend.smoke_test(&id, version.as_deref()))
        .await
        .map_err(|e| ApiError::internal(format!("frontend smoke-test task failed: {e}")))?
        .map_err(map_error)?;

    audit(
        &state,
        Some(user.id.to_string()),
        "frontend.smoke_test",
        None,
        json!({ "id": request.id, "passed": report.passed() }),
    )
    .await;
    Ok(Json(json!({
        "object": "smoke_report",
        "smoke": smoke_json(&report),
    })))
}

/// `POST /api/v1/frontend/reset` — revert to the bundled default
/// (admin).
pub async fn reset(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user = require_user(&caller)?;
    if !caller.is_admin() {
        return Err(ApiError::AdminRequired(user.id));
    }
    let info = state.frontend.reset();
    audit(
        &state,
        Some(user.id.to_string()),
        "frontend.reset",
        None,
        json!({ "id": info.id }),
    )
    .await;
    Ok(Json(serde_json::to_value(&info).unwrap_or_default()))
}

/// Map loader errors onto the §6.1 vocabulary.
fn map_error(error: FrontendError) -> ApiError {
    match error {
        FrontendError::NotFound(id) => ApiError::not_found("frontend package", id),
        FrontendError::AlreadyExists(id) => {
            ApiError::Conflict(format!("frontend package {id} is already uploaded"))
        }
        FrontendError::ApprovalRequired(signature) => ApiError::permission_denied_message(format!(
            "activation requires explicit admin approval: the package signature is \
             {signature} (re-send with \"approve_untrusted\": true after review)"
        )),
        FrontendError::SmokeTestFailed(report) => {
            ApiError::FrontendSmokeTestFailed { report: *report }
        }
        FrontendError::BadPackage(package) => {
            ApiError::validation(format!("package rejected: {package}"), "package")
        }
        FrontendError::Storage(message) => {
            ApiError::internal(format!("frontend storage error: {message}"))
        }
    }
}

/// Serialize a gate report for response bodies.
fn smoke_json(report: &SmokeReport) -> serde_json::Value {
    json!({
        "passed": report.passed(),
        "checks": report
            .checks
            .iter()
            .map(|check| {
                json!({
                    "name": check.name,
                    "passed": check.passed,
                    "detail": check.detail,
                })
            })
            .collect::<Vec<_>>(),
    })
}
