//! `/api/v1/users/*` — user CRUD and API-key management (spec §10.2).

use std::sync::Arc;

use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::error::ApiError;
use crate::handlers::{
    audit, conflict_on_backend, parse_grants, parse_uuid, query_params, require_admin,
    require_json_content_type, require_self_or_admin, require_user, user_json, ApiJson,
};
use crate::pagination::{page_envelope, PageParams};
use crate::state::{db_or_500, AppState, Caller};
use zim_engine_domain::{PermissionSet, User, UserId};

/// `GET /api/v1/users/me` — the current user.
pub async fn me(Extension(caller): Extension<Caller>) -> Result<Json<serde_json::Value>, ApiError> {
    let user = require_user(&caller)?;
    Ok(user_json(user))
}

/// Query shape for `GET /users`.
#[derive(Debug, Default, Deserialize)]
struct ListUsersQuery {
    page: Option<u32>,
    per_page: Option<u32>,
}

/// `GET /api/v1/users` — admin-only, page-based (§7).
pub async fn list(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    RawQuery(query): RawQuery,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&caller)?;
    let q: ListUsersQuery = query_params(query.as_deref().unwrap_or(""))?;
    let page = PageParams::from_query(q.page, q.per_page)?;

    let users = db_or_500(&state, |p| p.list_users()).await?;
    let total = users.len();
    let (start, end) = page.bounds(total);
    let data: Vec<_> = users[start..end].iter().map(|u| user_json(u).0).collect();
    Ok(Json(page_envelope(page, total, data)))
}

/// `POST /api/v1/users` — admin-created local accounts (spec §10.2;
/// self-registration is a later milestone per the roadmap).
#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    /// Login email (lowercased; unique).
    pub email: String,
    /// Display username (unique).
    pub username: String,
    /// Initial password (Argon2id-hashed at rest).
    pub password: String,
    /// Instance-administrator flag (default false).
    #[serde(default)]
    pub is_admin: bool,
    /// Root-administrator flag (only a root admin may create another).
    #[serde(default)]
    pub root_admin: bool,
}

/// `POST /api/v1/users`.
pub async fn create(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<CreateUserRequest>,
) -> Result<Response, ApiError> {
    let admin = require_admin(&caller)?;
    require_json_content_type(&headers)?;
    if request.root_admin && !admin.root_admin {
        return Err(ApiError::AdminRequired(admin.id));
    }

    let email = request.email.trim().to_lowercase();
    let user = User {
        id: UserId::new(),
        email,
        username: request.username,
        external_id: None,
        // Placeholder: the real Argon2id hash lands after structural
        // validation (below) rejects cheap failures first.
        password_hash: Some("pending-hash".to_owned()),
        oauth_subject: None,
        is_admin: request.is_admin,
        root_admin: request.root_admin,
    };
    // Structural validation before the expensive hash.
    user.validate()
        .map_err(|e| ApiError::validation(e.to_string(), "user"))?;

    // Uniqueness up front: duplicate email is a 409, not a 500.
    let probe_email = user.email.clone();
    let existing = db_or_500(&state, move |p| p.get_user_by_email(&probe_email)).await?;
    if existing.is_some() {
        return Err(ApiError::Conflict(format!(
            "a user with email {:?} already exists",
            user.email
        )));
    }

    // Argon2id hashing is CPU-heavy — off the async workers.
    let password = request.password;
    let policy = zim_engine_auth::PasswordPolicy::default();
    let hash = tokio::task::spawn_blocking(move || {
        zim_engine_auth::password::hash_password(&policy, &password)
    })
    .await
    .map_err(|e| ApiError::internal(format!("password hash task failed: {e}")))?
    .map_err(|e| ApiError::validation(format!("password rejected: {e}"), "password"))?;

    let mut user = user;
    user.password_hash = Some(hash);
    db_or_500(&state, {
        let user = user.clone();
        move |p| p.put_user(&user)
    })
    .await?;

    audit(
        &state,
        Some(admin.id.to_string()),
        "user.create",
        None,
        serde_json::json!({ "created": user.id.to_string(), "email": user.email }),
    )
    .await;

    let mut response = user_json(&user).into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

/// `GET /api/v1/users/{id}` — self or admin.
pub async fn get(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let target = parse_uuid::<UserId>(&id, "id")?;
    require_self_or_admin(&caller, &target)?;
    let user = db_or_500(&state, move |p| p.get_user(&target))
        .await?
        .ok_or_else(|| ApiError::not_found("user", id))?;
    Ok(user_json(&user))
}

/// `PATCH /api/v1/users/{id}` — self-service fields for everyone,
/// admin fields for admins (spec §10.2 "self or admin").
#[derive(Debug, Default, Deserialize)]
pub struct PatchUserRequest {
    /// New email (unique; validated).
    #[serde(default)]
    pub email: Option<String>,
    /// New username.
    #[serde(default)]
    pub username: Option<String>,
    /// New password (hashed; the old one is not required in M5 —
    /// loopback trust boundary, see the threat model).
    #[serde(default)]
    pub password: Option<String>,
    /// Instance-admin flag (admin-only change).
    #[serde(default)]
    pub is_admin: Option<bool>,
    /// Root-admin flag (root-admin-only change).
    #[serde(default)]
    pub root_admin: Option<bool>,
}

/// `PATCH /api/v1/users/{id}`.
pub async fn patch(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<PatchUserRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let caller_user = require_user(&caller)?;
    let target = parse_uuid::<UserId>(&id, "id")?;
    let is_self = caller_user.id == target;
    let is_admin = caller.is_admin();
    if !is_self && !is_admin {
        return Err(ApiError::AdminRequired(caller_user.id));
    }
    require_json_content_type(&headers)?;

    // Privilege fields: admins only; root flag: root only.
    if (request.is_admin.is_some() || request.root_admin.is_some()) && !is_admin {
        return Err(ApiError::AdminRequired(caller_user.id));
    }
    if request.root_admin.is_some() && !caller_user.root_admin {
        return Err(ApiError::AdminRequired(caller_user.id));
    }

    let user = db_or_500(&state, move |p| p.get_user(&target))
        .await?
        .ok_or_else(|| ApiError::not_found("user", id))?;
    let mut updated = user.clone();

    if let Some(email) = request.email {
        updated.email = email.trim().to_lowercase();
    }
    if let Some(username) = request.username {
        updated.username = username;
    }
    if let Some(is_admin_flag) = request.is_admin {
        updated.is_admin = is_admin_flag;
    }
    if let Some(root_admin_flag) = request.root_admin {
        updated.root_admin = root_admin_flag;
    }
    if let Some(password) = request.password {
        let policy = zim_engine_auth::PasswordPolicy::default();
        let hash = tokio::task::spawn_blocking(move || {
            zim_engine_auth::password::hash_password(&policy, &password)
        })
        .await
        .map_err(|e| ApiError::internal(format!("password hash task failed: {e}")))?
        .map_err(|e| ApiError::validation(format!("password rejected: {e}"), "password"))?;
        updated.password_hash = Some(hash);
    }
    updated
        .validate()
        .map_err(|e| ApiError::validation(e.to_string(), "user"))?;

    // Email uniqueness when it moved.
    if updated.email != user.email {
        let probe = updated.email.clone();
        let clash = db_or_500(&state, move |p| p.get_user_by_email(&probe)).await?;
        if clash.is_some_and(|other| other.id != user.id) {
            return Err(ApiError::Conflict(format!(
                "a user with email {:?} already exists",
                updated.email
            )));
        }
    }

    db_or_500(&state, {
        let updated = updated.clone();
        move |p| p.put_user(&updated)
    })
    .await?;
    audit(
        &state,
        Some(caller_user.id.to_string()),
        "user.update",
        None,
        serde_json::json!({ "target": target.to_string() }),
    )
    .await;
    Ok(user_json(&updated))
}

/// `DELETE /api/v1/users/{id}` — admin-only; root admins are
/// undeletable (domain invariant); owners with instances conflict.
pub async fn delete(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let admin = require_admin(&caller)?;
    let target = parse_uuid::<UserId>(&id, "id")?;
    let user = db_or_500(&state, move |p| p.get_user(&target))
        .await?
        .ok_or_else(|| ApiError::not_found("user", id))?;
    if user.root_admin {
        return Err(ApiError::Conflict(
            "the root administrator cannot be deleted (bootstrap invariant)".to_owned(),
        ));
    }
    if user.id == admin.id {
        return Err(ApiError::Conflict(
            "cannot delete your own account".to_owned(),
        ));
    }

    crate::state::db(&state, move |p| p.delete_user(&target))
        .await
        .map_err(|e| {
            conflict_on_backend(
                e,
                "user still owns instances; reassign or delete them first",
            )
        })?;
    // Sessions and keys die with the account.
    state.sessions.revoke_all_for_user(&target);
    audit(
        &state,
        Some(admin.id.to_string()),
        "user.delete",
        None,
        serde_json::json!({ "deleted": target.to_string() }),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

// ── API keys (§10.2) ──────────────────────────────────────────────

/// The JSON shape for an API key record (hashes never leave the
/// engine).
fn api_key_json(record: &zim_engine_auth::ApiKeyRecord) -> serde_json::Value {
    serde_json::json!({
        "key_id": record.key_id,
        "user_id": record.user_id,
        "scopes": record.scopes.iter().map(|g| g.as_str()).collect::<Vec<_>>(),
        "created_at": record.created_at,
        "expires_at": record.expires_at,
        "last_used_at": record.last_used_at,
    })
}

/// `GET /api/v1/users/{id}/api-keys` — self or admin.
pub async fn list_api_keys(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let target = parse_uuid::<UserId>(&id, "id")?;
    require_self_or_admin(&caller, &target)?;
    let records = state.api_keys.list_for_user(&target);
    let data: Vec<_> = records.iter().map(api_key_json).collect();
    Ok(Json(serde_json::json!({ "data": data })))
}

/// `POST /api/v1/users/{id}/api-keys` — issue a scoped key.
///
/// Scope rule (the auth crate delegates issuance-time enforcement
/// here): the requested scopes must be a subset of what the target
/// user can exercise — admins: the vocabulary; plain users: the
/// union of their instance permission rows.
#[derive(Debug, Deserialize)]
pub struct CreateApiKeyRequest {
    /// Requested scope grants (dotted names).
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Optional expiry (ISO 8601).
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

/// `POST /api/v1/users/{id}/api-keys`.
pub async fn create_api_key(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<CreateApiKeyRequest>,
) -> Result<Response, ApiError> {
    let caller_user = require_user(&caller)?;
    let target = parse_uuid::<UserId>(&id, "id")?;
    require_self_or_admin(&caller, &target)?;

    let grants = parse_grants(&request.scopes)?;
    let user = db_or_500(&state, move |p| p.get_user(&target))
        .await?
        .ok_or_else(|| ApiError::not_found("user", id))?;

    if !(user.root_admin || user.is_admin) {
        // Plain user: scopes must be covered by their permission rows.
        let rows = db_or_500(&state, move |p| p.list_permissions_by_user(&target)).await?;
        let mut owned = PermissionSet::new();
        for row in rows {
            for grant in row.grants.iter() {
                owned.grant(grant);
            }
        }
        let missing: Vec<_> = grants
            .iter()
            .copied()
            .filter(|g| !owned.contains(*g))
            .collect();
        if !missing.is_empty() {
            return Err(ApiError::PermissionDenied {
                message: "requested API key scopes exceed the user's own grants".to_owned(),
                missing,
                user_id: target,
            });
        }
    }

    let mut scopes = PermissionSet::new();
    for grant in grants {
        scopes.grant(grant);
    }
    let issued = state
        .api_keys
        .issue(&target, scopes, request.expires_at, Utc::now());

    audit(
        &state,
        Some(caller_user.id.to_string()),
        "user.api_key.create",
        None,
        serde_json::json!({ "target": target.to_string(), "key_id": issued.record.key_id }),
    )
    .await;

    // The full key is shown exactly once (spec §3 / auth crate).
    let mut body = api_key_json(&issued.record);
    body["key"] = serde_json::json!(issued.full_key);
    let mut response = Json(serde_json::json!({ "data": body })).into_response();
    *response.status_mut() = StatusCode::CREATED;
    Ok(response)
}

/// `DELETE /api/v1/users/{id}/api-keys/{keyId}` — revoke (self or
/// admin).
pub async fn revoke_api_key(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((id, key_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let caller_user = require_user(&caller)?;
    let target = parse_uuid::<UserId>(&id, "id")?;
    require_self_or_admin(&caller, &target)?;
    // Ownership check: the key must belong to the target user.
    let records = state.api_keys.list_for_user(&target);
    let belongs = records.iter().any(|r| r.key_id == key_id);
    if !belongs {
        return Err(ApiError::not_found("api key", key_id));
    }
    state
        .api_keys
        .revoke(&key_id)
        .map_err(|_| ApiError::not_found("api key", key_id.clone()))?;
    audit(
        &state,
        Some(caller_user.id.to_string()),
        "user.api_key.revoke",
        None,
        serde_json::json!({ "target": target.to_string(), "key_id": key_id }),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
