//! The packaged-theme family (M23's deferred upload surface,
//! delivered with M24): `GET/POST/DELETE /api/v1/themes`.
//!
//! Themes are cosmetic, not authority — but installing one changes
//! what every page renders, so upload/delete are admin while the
//! list is any session (the theme panel every user sees).

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::header;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use zim_engine_extensions::{ThemeError, ThemeService};

use crate::error::ApiError;
use crate::handlers::{audit, require_user};
use crate::state::{AppState, Caller};

/// `GET /api/v1/themes` — the uploaded themes (any session; the
/// compiled-in presets live in the frontend, not here).
pub async fn list(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_user(&caller)?;
    let themes = Arc::clone(&state.themes);
    let summaries = tokio::task::spawn_blocking(move || themes.list())
        .await
        .map_err(|e| ApiError::internal(format!("theme list task failed: {e}")))?;
    Ok(Json(serde_json::json!({ "data": summaries })))
}

/// `POST /api/v1/themes` — install a packaged theme from
/// `application/zip` bytes with `zim.theme.json` at the root
/// (admin).
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

    let themes: Arc<ThemeService> = Arc::clone(&state.themes);
    let bytes = body.to_vec();
    let outcome = tokio::task::spawn_blocking(move || themes.upload(&bytes))
        .await
        .map_err(|e| ApiError::internal(format!("theme upload task failed: {e}")))?;

    match outcome {
        Ok(summary) => {
            audit(
                &state,
                Some(user.id.to_string()),
                "theme.upload",
                None,
                serde_json::json!({
                    "id": summary.id,
                    "name": summary.name,
                    "overrides": summary.overrides,
                }),
            )
            .await;
            let mut response = Json(serde_json::to_value(&summary).unwrap_or_default())
                .into_response();
            *response.status_mut() = axum::http::StatusCode::CREATED;
            Ok(response)
        }
        Err(error) => Err(map_error(error)),
    }
}

/// `DELETE /api/v1/themes/{id}` — remove an uploaded theme (admin).
pub async fn delete(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let user = require_user(&caller)?;
    if !caller.is_admin() {
        return Err(ApiError::AdminRequired(user.id));
    }
    let themes: Arc<ThemeService> = Arc::clone(&state.themes);
    let theme_id = id.clone();
    let outcome = tokio::task::spawn_blocking(move || themes.delete(&theme_id))
        .await
        .map_err(|e| ApiError::internal(format!("theme delete task failed: {e}")))?;

    match outcome {
        Ok(()) => {
            audit(
                &state,
                Some(user.id.to_string()),
                "theme.delete",
                None,
                serde_json::json!({ "id": id }),
            )
            .await;
            Ok(axum::http::StatusCode::NO_CONTENT.into_response())
        }
        Err(error) => Err(map_error(error)),
    }
}

/// Map the theme vocabulary onto §6.1.
fn map_error(error: ThemeError) -> ApiError {
    match error {
        ThemeError::BadArchive(message) => {
            ApiError::validation(format!("package rejected: {message}"), "package")
        }
        ThemeError::BadTheme(message) => {
            ApiError::validation(message, "zim.theme.json")
        }
        ThemeError::AlreadyExists(name) => {
            ApiError::Conflict(format!("a theme named {name} is already installed"))
        }
        ThemeError::Storage(message) => {
            ApiError::internal(format!("theme storage error: {message}"))
        }
    }
}
