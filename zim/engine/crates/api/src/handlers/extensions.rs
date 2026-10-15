//! The §10.12 extension family — the real M24 handlers over
//! `ExtensionsService` (they replace the M5-era 501 stubs).
//!
//! - `GET /extensions` — every installed addon with its
//!   contributions and notices (any session).
//! - `POST /extensions` — install/update from ZIP bytes (admin;
//!   §4.11: the admin's upload IS the approval for
//!   untrusted/unsigned packages, a bad trusted-key signature is a
//!   hard reject).
//! - `DELETE /extensions/{id}` — uninstall + permission revocation
//!   (admin; §6's bundled defaults are disable-only).
//! - `POST /extensions/{id}/enable` / `disable` — admin.
//! - `POST /extensions/{id}/grant` — grant additional permissions
//!   (admin; §4.2's elevation approval).
//!
//! Addon lifecycle moves are audited — they are authority moves on
//! the engine's trust surface, exactly like frontend activations.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::header;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use zim_engine_extensions::ExtensionError;

use crate::error::ApiError;
use crate::handlers::{audit, require_user};
use crate::state::{AppState, Caller};

/// `GET /api/v1/extensions` — list installed addons (any session).
pub async fn list(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_user(&caller)?;
    let extensions = Arc::clone(&state.extensions);
    let (summaries, notices) =
        tokio::task::spawn_blocking(move || (extensions.list(), extensions.boot_notices()))
            .await
            .map_err(|e| ApiError::internal(format!("extension list task failed: {e}")))?;
    Ok(Json(serde_json::json!({
        "data": summaries,
        "notices": notices,
    })))
}

/// `POST /api/v1/extensions` — install (or update) an addon from
/// `application/zip` bytes (admin).
pub async fn install(
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

    let extensions = Arc::clone(&state.extensions);
    let bytes = body.to_vec();
    let byte_count = bytes.len();
    let outcome = tokio::task::spawn_blocking(move || extensions.install(&bytes))
        .await
        .map_err(|e| ApiError::internal(format!("extension install task failed: {e}")))?;

    match outcome {
        Ok(summary) => {
            audit(
                &state,
                Some(user.id.to_string()),
                "extension.install",
                None,
                serde_json::json!({
                    "id": summary.id,
                    "version": summary.version,
                    "signature": summary.signature,
                    "permissions": summary.permissions.requested,
                    "bytes": byte_count,
                }),
            )
            .await;
            let mut response =
                Json(serde_json::to_value(&summary).unwrap_or_default()).into_response();
            *response.status_mut() = axum::http::StatusCode::CREATED;
            Ok(response)
        }
        Err(error) => Err(map_error(error)),
    }
}

/// `DELETE /api/v1/extensions/{id}` — uninstall + revoke (admin).
pub async fn uninstall(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let user = require_user(&caller)?;
    if !caller.is_admin() {
        return Err(ApiError::AdminRequired(user.id));
    }
    let extensions = Arc::clone(&state.extensions);
    let addon_id = id.clone();
    let outcome = tokio::task::spawn_blocking(move || extensions.uninstall(&addon_id))
        .await
        .map_err(|e| ApiError::internal(format!("extension uninstall task failed: {e}")))?;

    match outcome {
        Ok(()) => {
            audit(
                &state,
                Some(user.id.to_string()),
                "extension.uninstall",
                None,
                serde_json::json!({ "id": id }),
            )
            .await;
            Ok(axum::http::StatusCode::NO_CONTENT.into_response())
        }
        Err(error) => Err(map_error(error)),
    }
}

/// `POST /api/v1/extensions/{id}/enable` — admin.
pub async fn enable(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    lifecycle(&state, &caller, &id, Lifecycle::Enable).await
}

/// `POST /api/v1/extensions/{id}/disable` — admin.
pub async fn disable(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    lifecycle(&state, &caller, &id, Lifecycle::Disable).await
}

enum Lifecycle {
    Enable,
    Disable,
}

impl Lifecycle {
    fn action(&self) -> &'static str {
        match self {
            Self::Enable => "extension.enable",
            Self::Disable => "extension.disable",
        }
    }
}

async fn lifecycle(
    state: &Arc<AppState>,
    caller: &Caller,
    id: &str,
    which: Lifecycle,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user = require_user(caller)?;
    if !caller.is_admin() {
        return Err(ApiError::AdminRequired(user.id));
    }
    let action = which.action();
    let extensions = Arc::clone(&state.extensions);
    let addon_id = id.to_owned();
    let outcome = tokio::task::spawn_blocking(move || match which {
        Lifecycle::Enable => extensions.enable(&addon_id),
        Lifecycle::Disable => extensions.disable(&addon_id),
    })
    .await
    .map_err(|e| ApiError::internal(format!("extension lifecycle task failed: {e}")))?;

    match outcome {
        Ok(summary) => {
            audit(
                state,
                Some(user.id.to_string()),
                action,
                None,
                serde_json::json!({ "id": summary.id, "version": summary.version }),
            )
            .await;
            Ok(Json(serde_json::to_value(&summary).unwrap_or_default()))
        }
        Err(error) => Err(map_error(error)),
    }
}

/// `POST /api/v1/extensions/{id}/grant` — grant additional
/// permissions (admin; §4.2's re-approval for elevation).
#[derive(Debug, Deserialize)]
pub struct GrantRequest {
    /// The dotted permission names to grant.
    pub permissions: Vec<String>,
}

/// The grant handler: validates the names, merges them into the
/// registration's granted set, and audits the elevation.
pub async fn grant(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(request): Json<GrantRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user = require_user(&caller)?;
    if !caller.is_admin() {
        return Err(ApiError::AdminRequired(user.id));
    }
    if request.permissions.is_empty() {
        return Err(ApiError::validation(
            "the request body must list at least one permission",
            "permissions",
        ));
    }
    let extensions = Arc::clone(&state.extensions);
    let addon_id = id.clone();
    let permissions = request.permissions.clone();
    let outcome = tokio::task::spawn_blocking(move || extensions.grant(&addon_id, &permissions))
        .await
        .map_err(|e| ApiError::internal(format!("extension grant task failed: {e}")))?;

    match outcome {
        Ok(summary) => {
            audit(
                &state,
                Some(user.id.to_string()),
                "extension.grant",
                None,
                serde_json::json!({
                    "id": summary.id,
                    "granted": request.permissions,
                    "total_granted": summary.permissions.granted,
                }),
            )
            .await;
            Ok(Json(serde_json::to_value(&summary).unwrap_or_default()))
        }
        Err(error) => Err(map_error(error)),
    }
}

/// Map the service's error vocabulary onto §6.1 (the frontend
/// handler's discipline: shape failures are validation errors,
/// lifecycle conflicts are 409s, storage failures are 500s).
fn map_error(error: ExtensionError) -> ApiError {
    match error {
        ExtensionError::NotFound(id) => ApiError::not_found("addon", id),
        ExtensionError::AlreadyExists(id, version) => {
            ApiError::Conflict(format!("addon {id} version {version} is already installed"))
        }
        ExtensionError::DowngradeRefused(id, have, wanted) => ApiError::Conflict(format!(
            "addon {id} is at {have}; refusing downgrade to {wanted}"
        )),
        ExtensionError::HasDependents(id, dependent) => ApiError::Conflict(format!(
            "addon {id} is required by {dependent}; uninstall the dependent first"
        )),
        ExtensionError::BundledNotRemovable(id) => ApiError::permission_denied_message(format!(
            "{id} is a bundled default addon (§6): it can be disabled, not removed"
        )),
        ExtensionError::BadSignature => ApiError::permission_denied_message(
            "the addon's signature is invalid (signed by a trusted key but it does not verify); \
             no approval can override this"
                .to_owned(),
        ),
        ExtensionError::SurfaceCollision { surface, owner } => {
            ApiError::Conflict(format!("page surface {surface} collides with {owner}"))
        }
        ExtensionError::DependencyUnsatisfied(dep) => ApiError::validation(
            format!("dependency {dep} is not satisfied by any installed package"),
            "dependencies",
        ),
        ExtensionError::ApiGeneration(wanted, have) => ApiError::validation(
            format!("the addon speaks api_version {wanted}, this engine speaks {have}"),
            "api_version",
        ),
        ExtensionError::EngineIncompatible(wanted, have) => ApiError::validation(
            format!("the addon requires zim {wanted}, this engine is {have}"),
            "engines.zim",
        ),
        ExtensionError::UnknownPermission(name) => ApiError::validation(
            format!("{name} is not in the permission vocabulary"),
            "permissions",
        ),
        ExtensionError::BadPackage(package) => {
            ApiError::validation(format!("package rejected: {package}"), "package")
        }
        ExtensionError::BadManifest(manifest) => {
            ApiError::validation(format!("manifest rejected: {manifest}"), "manifest")
        }
        ExtensionError::Storage(message) => {
            ApiError::internal(format!("addon storage error: {message}"))
        }
        ExtensionError::Persistence(error) => ApiError::from(error),
    }
}
