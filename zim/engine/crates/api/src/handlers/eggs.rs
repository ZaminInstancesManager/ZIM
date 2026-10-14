//! `/api/v1/eggs/*` — the §10.11 egg surface: list, import
//! (legacy egg JSON or `zim.toml`), get, update, delete.
//!
//! The import composes the pure importer (`zim_engine_eggs`) with
//! the store: an egg whose legacy UUID already exists updates that
//! row in place (idempotent re-import, the Pterodactyl flow
//! operators expect); otherwise a new egg is created. The
//! security-relevant feature flags require the explicit
//! `allow_privileged_install_scripts` / `allow_requires_root`
//! opt-ins in the request body — the importer rejects them by
//! default.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::Utc;
use serde::Deserialize;

use crate::error::ApiError;
use crate::handlers::{audit, parse_uuid, require_admin, require_user, ApiJson};
use crate::state::{db_or_500, AppState, Caller};
use zim_engine_domain::EggId;

// ── Import request shape ──────────────────────────────────────────

/// `POST /eggs` body: either a bare legacy egg JSON document
/// (detected by its `startup` field — every legacy egg has one) or
/// a wrapper that names the format, carries the document as a
/// string, and holds the security opt-ins.
/// The wrapper form of `POST /eggs` (the bare legacy egg document
/// is the other form; see [`import_egg`]).
#[derive(Debug, Deserialize)]
struct ImportWrapper {
    /// `legacy_json` (default) or `zim_toml`.
    #[serde(default = "default_format")]
    format: String,
    /// The egg document as a string (the legacy JSON or the
    /// `zim.toml` text).
    source: String,
    /// The explicit admin opt-in for privileged install scripts.
    #[serde(default)]
    allow_privileged_install_scripts: bool,
    /// The explicit admin opt-in for root-requiring workloads.
    #[serde(default)]
    allow_requires_root: bool,
}

fn default_format() -> String {
    "legacy_json".to_owned()
}

// ── Handlers ──────────────────────────────────────────────────────

/// `GET /api/v1/eggs` — list eggs (spec §10.11; any session). The
/// listing is trimmed (the full definition — scripts, config
/// rules, variables — comes from `GET /eggs/{id}`).
pub async fn list_eggs(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_user(&caller)?;
    let eggs = db_or_500(&state, |p| p.list_eggs()).await?;
    let data: Vec<serde_json::Value> = eggs
        .iter()
        .map(|egg| {
            serde_json::json!({
                "id": egg.id,
                "legacy_uuid": egg.legacy_uuid,
                "name": egg.name,
                "description": egg.description,
                "author": egg.author,
                "images": egg.images.iter()
                    .map(|image| serde_json::json!({
                        "label": image.label,
                        "image": image.image.to_string(),
                    }))
                    .collect::<Vec<_>>(),
                "variable_count": egg.variables.len(),
                "has_install_script": egg.scripts.install.is_some(),
                "created_at": egg.created_at,
                "updated_at": egg.updated_at,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "data": data })))
}

/// `POST /api/v1/eggs` — import an egg (spec §10.11; admin).
///
/// A bare JSON body is a legacy egg document; the wrapper form
/// names the format (`legacy_json` | `zim_toml`) and carries the
/// security opt-ins. Answers `201` with the imported egg and the
/// importer's warnings; a re-import of a known legacy UUID updates
/// that egg in place and answers `200`.
pub async fn import_egg(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    ApiJson(body): ApiJson<serde_json::Value>,
) -> Result<Response, ApiError> {
    let admin = require_admin(&caller)?;

    // Unwrap the request into (format, source, policy).
    let (format, source, policy) = {
        let is_wrapper = body
            .as_object()
            .is_some_and(|map| map.contains_key("source") || map.contains_key("format"));
        if is_wrapper {
            let wrapper: ImportWrapper = serde_json::from_value(body)
                .map_err(|e| ApiError::validation(e.to_string(), "body"))?;
            (
                wrapper.format,
                wrapper.source,
                zim_engine_eggs::ImportPolicy {
                    allow_privileged_install_scripts: wrapper.allow_privileged_install_scripts,
                    allow_requires_root: wrapper.allow_requires_root,
                },
            )
        } else {
            // A bare legacy egg: every legacy document carries a
            // `startup` field; the default policy is strict.
            if body.get("startup").is_none() {
                return Err(ApiError::validation(
                    "the body is neither a legacy egg document (no \"startup\" field) nor a \
                     wrapped import ({\"format\": ..., \"source\": ...})",
                    "body",
                ));
            }
            (
                "legacy_json".to_owned(),
                serde_json::to_string(&body).unwrap_or_default(),
                zim_engine_eggs::ImportPolicy::default(),
            )
        }
    };

    // Import.
    let report = match format.as_str() {
        "legacy_json" => zim_engine_eggs::import_legacy_json(&source, &policy),
        "zim_toml" => zim_engine_eggs::import_zim_toml(&source, &policy),
        other => {
            return Err(ApiError::validation(
                format!(
                    "unknown import format {other:?}; expected \"legacy_json\" or \"zim_toml\""
                ),
                "format",
            ))
        }
    }
    .map_err(egg_import_error)?;

    let mut egg = report.egg;
    let imported_name = egg.name.clone();

    // Deduplicate on the legacy UUID: a re-import updates the
    // existing row in place (the operator's mental model).
    let existing = match egg.legacy_uuid.clone() {
        Some(uuid) => db_or_500(&state, move |p| p.get_egg_by_legacy_uuid(&uuid)).await?,
        None => None,
    };
    let updating = existing.is_some();
    if let Some(existing) = existing {
        egg.id = existing.id;
        egg.created_at = existing.created_at;
    }
    let persisted_id = egg.id;
    db_or_500(&state, move |p| p.put_egg(&egg)).await?;

    audit(
        &state,
        Some(admin.id.to_string()),
        if updating {
            "egg.import.update"
        } else {
            "egg.import"
        },
        None,
        serde_json::json!({
            "egg_id": persisted_id,
            "name": imported_name,
            "format": format,
            "warnings": report.warnings.len(),
        }),
    )
    .await;

    let egg = db_or_500(&state, move |p| p.get_egg(&persisted_id))
        .await?
        .ok_or_else(|| ApiError::internal("the imported egg vanished on read-back"))?;
    let mut response = Json(serde_json::json!({
        "egg": serde_json::to_value(&egg).unwrap_or_default(),
        "warnings": report.warnings,
        "updated_existing": updating,
    }))
    .into_response();
    *response.status_mut() = if updating {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok(response)
}

/// `GET /api/v1/eggs/{id}` — the full egg definition (spec §10.11;
/// any session).
pub async fn get_egg(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_user(&caller)?;
    let egg_id = parse_uuid::<EggId>(&id, "id")?;
    let egg = db_or_500(&state, move |p| p.get_egg(&egg_id))
        .await?
        .ok_or_else(|| ApiError::not_found("egg", id))?;
    Ok(Json(serde_json::to_value(&egg).unwrap_or_default()))
}

/// `PATCH /api/v1/eggs/{id}` — update an egg's metadata (spec
/// §10.11; admin): name, description, author, and the startup
/// template. (Variable and script edits re-import the egg — the
/// document formats are the editing surface for those.)
#[derive(Debug, Default, Deserialize)]
pub struct EggPatchBody {
    /// New display name.
    #[serde(default)]
    pub name: Option<String>,
    /// New description.
    #[serde(default)]
    pub description: Option<String>,
    /// New author.
    #[serde(default)]
    pub author: Option<String>,
    /// New startup command template.
    #[serde(default)]
    pub startup: Option<String>,
}

/// `PATCH /api/v1/eggs/{id}` — see [`EggPatchBody`].
pub async fn patch_egg(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<EggPatchBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let admin = require_admin(&caller)?;
    let egg_id = parse_uuid::<EggId>(&id, "id")?;
    let mut egg = db_or_500(&state, move |p| p.get_egg(&egg_id))
        .await?
        .ok_or_else(|| ApiError::not_found("egg", id))?;
    if let Some(name) = body.name {
        egg.name = name;
    }
    if let Some(description) = body.description {
        egg.description = description;
    }
    if let Some(author) = body.author {
        egg.author = author;
    }
    if let Some(startup) = body.startup {
        egg.startup = zim_engine_domain::CommandTemplate::new(startup);
    }
    egg.updated_at = Utc::now();
    egg.validate()
        .map_err(|e| ApiError::validation(e.to_string(), "egg"))?;
    let row = egg.clone();
    db_or_500(&state, move |p| p.put_egg(&row)).await?;
    audit(
        &state,
        Some(admin.id.to_string()),
        "egg.update",
        None,
        serde_json::json!({ "egg_id": egg.id, "name": egg.name }),
    )
    .await;
    Ok(Json(serde_json::to_value(&egg).unwrap_or_default()))
}

/// `DELETE /api/v1/eggs/{id}` — delete an egg (spec §10.11;
/// admin). Refuses with `409 conflict` while instances still run
/// the egg.
pub async fn delete_egg(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let admin = require_admin(&caller)?;
    let egg_id = parse_uuid::<EggId>(&id, "id")?;
    let instances = db_or_500(&state, move |p| p.list_instances_by_egg(&egg_id)).await?;
    if !instances.is_empty() {
        return Err(ApiError::Conflict(format!(
            "the egg still backs {} instance(s); delete or move them first",
            instances.len()
        )));
    }
    db_or_500(&state, move |p| p.delete_egg(&egg_id)).await?;
    audit(
        &state,
        Some(admin.id.to_string()),
        "egg.delete",
        None,
        serde_json::json!({ "egg_id": egg_id }),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

// ── Error mapping ─────────────────────────────────────────────────

/// Map an importer failure onto the §6.1 error vocabulary.
fn egg_import_error(e: zim_engine_eggs::EggImportError) -> ApiError {
    use zim_engine_eggs::EggImportError as E;
    match e {
        E::Json(message) | E::Toml(message) => ApiError::validation(message, "source"),
        E::MissingName | E::MissingStartup => ApiError::validation(e.to_string(), "source"),
        E::PrivilegedInstallScripts | E::RequiresRoot => ApiError::Conflict(e.to_string()),
        E::Domain(_) => ApiError::validation(e.to_string(), "source"),
    }
}
