//! Endpoint handlers and the guard helpers they share.
//!
//! Guards (small `require_*` functions called at the top of each
//! handler) are the M5 realization of the M4 "middleware that
//! extracts the credential, resolves the PermissionSet, checks
//! required grants" contract: the auth middleware resolves the
//! caller once per request; guards map route requirements onto
//! `zim_engine_authz::require` and produce the structured `401` /
//! `403` responses.

pub mod audit;
pub mod auth;
pub mod eggs;
pub mod frontend;
pub mod instances;
pub mod misc;
pub mod nodes;
pub mod pages;
pub mod recents;
pub mod stream;
pub mod users;

use std::sync::Arc;

use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use zim_engine_application::ports::PersistenceError;
use zim_engine_authz::{require, resolve_instance_grants, AuthzContext};
use zim_engine_domain::{
    Instance, InstanceId, Permission, PermissionGrant, PermissionSet, User, UserId,
};

use crate::error::ApiError;
use crate::state::{db_or_500, AppState, Caller};

/// Parse a UUID-shaped path or body field into a typed id.
pub fn parse_uuid<T>(raw: &str, field: &'static str) -> Result<T, ApiError>
where
    T: std::str::FromStr<Err = uuid::Error>,
{
    raw.parse::<T>()
        .map_err(|e| ApiError::validation(format!("invalid UUID for {field}: {e}"), field))
}

/// Parse a list of dotted grant names (subuser / API-key scopes).
///
/// Unknown names produce a `validation_failed` listing every
/// offender, so clients can fix the whole batch at once.
pub fn parse_grants(raw: &[String]) -> Result<Vec<PermissionGrant>, ApiError> {
    let mut grants = Vec::with_capacity(raw.len());
    let mut unknown = Vec::new();
    for name in raw {
        match PermissionGrant::ALL
            .iter()
            .copied()
            .find(|g| g.as_str() == name)
        {
            Some(g) => grants.push(g),
            None => unknown.push(name.clone()),
        }
    }
    if unknown.is_empty() {
        Ok(grants)
    } else {
        Err(ApiError::validation(
            format!(
                "unknown permission grants: {} (see the PermissionGrant vocabulary)",
                unknown.join(", ")
            ),
            "grants",
        ))
    }
}

/// Guard: the request must carry a credential (`401` otherwise).
pub fn require_user(caller: &Caller) -> Result<&User, ApiError> {
    caller.require_user()
}

/// Guard: the caller must hold an admin flag (spec §10 "admin"
/// columns; `403 permission_denied` otherwise).
pub fn require_admin(caller: &Caller) -> Result<&User, ApiError> {
    let user = caller.require_user()?;
    if caller.is_admin() {
        Ok(user)
    } else {
        Err(ApiError::AdminRequired(user.id))
    }
}

/// Guard: the target user must be the caller or an admin (`403`
/// otherwise). Missing user rows are the caller's problem to
/// distinguish (handlers check existence first when it matters).
pub fn require_self_or_admin<'a>(
    caller: &'a Caller,
    target: &UserId,
) -> Result<&'a User, ApiError> {
    let user = caller.require_user()?;
    if caller.is_admin() || user.id == *target {
        Ok(user)
    } else {
        Err(ApiError::AdminRequired(user.id))
    }
}

/// Load an instance the caller is allowed to see.
///
/// Visibility (spec §10.3 "any instance grant"): owner, admin, or a
/// permission row. Anything else — including nonexistent ids — is a
/// uniform `404` so instance existence is not leaked to strangers.
///
/// Returns the row plus the caller's resolved authz context for it
/// (the same shape the M4 crate produces; guards then call
/// [`require`]).
pub async fn load_instance_for_caller(
    state: &AppState,
    caller: &Caller,
    instance_id: &InstanceId,
) -> Result<(Instance, AuthzContext), ApiError> {
    let user = caller.require_user()?.clone();
    let id = *instance_id;
    let instance = db_or_500(state, move |p| p.get_instance(&id))
        .await?
        .ok_or_else(|| ApiError::not_found("instance", instance_id))?;

    let owner = instance.owner_id == user.id;
    let admin = user.root_admin || user.is_admin;
    let mut permission_row = None;
    if !owner && !admin {
        permission_row = db_or_500(state, {
            let id = *instance_id;
            let uid = user.id;
            move |p| p.get_permission(&uid, &id)
        })
        .await?;
        if permission_row.is_none() {
            // Uniform 404: no row, no visibility.
            return Err(ApiError::not_found("instance", instance_id));
        }
    }

    // M6: an instance owner is implicitly authorized on their own
    // instance (Pterodactyl semantics — subuser rows exist for
    // *other* users). The effective set for a non-admin owner is the
    // full grant vocabulary minus the admin-only `AdminInstance`
    // grant (delete/suspend stay admin actions per §10.3).
    let grants = if owner && !admin {
        owner_implicit_grants()
    } else {
        resolve_instance_grants(&user, permission_row.as_ref())
    };
    let ctx = AuthzContext::for_user(&user, grants, *instance_id);
    Ok((instance, ctx))
}

/// The owner's implicit grant set: everything except the
/// admin-scoped `AdminInstance` grant.
fn owner_implicit_grants() -> PermissionSet {
    let mut set = PermissionSet::new();
    for grant in PermissionGrant::ALL {
        if *grant != PermissionGrant::AdminInstance {
            set.grant(*grant);
        }
    }
    set
}

/// Guard: run [`require`] and map the authz error into the API's
/// structured `permission_denied`.
pub fn check_grants(ctx: &AuthzContext, required: &[PermissionGrant]) -> Result<(), ApiError> {
    require(ctx, required).map_err(|e| match e {
        zim_engine_authz::AuthzError::PermissionDenied { missing, user_id } => {
            ApiError::PermissionDenied {
                message: format!(
                    "this operation requires the {} permission{}",
                    missing
                        .iter()
                        .map(|g| g.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    if missing.len() > 1 { "s" } else { "" }
                ),
                missing,
                user_id,
            }
        }
    })
}

/// Map a persistence failure that may be an expected FK rejection
/// into `409 conflict` (delete-parent-with-children and friends);
/// everything else stays an `internal_error`.
pub fn conflict_on_backend(error: PersistenceError, message: &str) -> ApiError {
    match error {
        PersistenceError::Backend(reason) if reason.contains("FOREIGN KEY") => {
            ApiError::Conflict(message.to_owned())
        }
        other => ApiError::from(other),
    }
}

/// Append an audit record (best-effort: audit failures are logged
/// and swallowed — losing the trail must not fail the user-visible
/// operation; the append-only log keeps no partial state).
pub async fn audit(
    state: &Arc<AppState>,
    actor_id: Option<String>,
    action: &str,
    instance_id: Option<InstanceId>,
    metadata: serde_json::Value,
) {
    let record = zim_engine_domain::AuditRecord {
        id: 0,
        timestamp: chrono::Utc::now(),
        actor: zim_engine_domain::AuditActor::User,
        actor_id,
        action: action.to_owned(),
        instance_id,
        metadata,
        ip_address: None,
        user_agent: None,
    };
    let result = db_or_500(state, move |p| p.append_audit(&record)).await;
    if let Err(e) = result {
        eprintln!("zim_engine_api: audit append failed for {action:?}: {e}");
    }
}

/// Extract typed query parameters via serde over the raw query map.
///
/// Unknown keys are ignored (`filter[...]`-style keys arrive later);
/// malformed typed values produce `validation_failed`.
pub fn query_params<T>(query: &str) -> Result<T, ApiError>
where
    T: serde::de::DeserializeOwned + Default,
{
    if query.is_empty() {
        return Ok(T::default());
    }
    // form_urlencoded-shaped pairs — axum's Query uses the same
    // convention; parse manually to keep the error mapping ours.
    // Values are coerced like serde_urlencoded: integers, floats and
    // booleans become JSON numbers/bools so typed target structs see
    // the right shapes.
    let mut map = serde_json::Map::new();
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let decoded = percent_decode(v);
        let value = if let Ok(n) = decoded.parse::<i64>() {
            serde_json::Value::Number(n.into())
        } else if let Ok(f) = decoded.parse::<f64>() {
            serde_json::Number::from_f64(f)
                .map(serde_json::Value::Number)
                .unwrap_or_else(|| serde_json::Value::String(decoded.clone()))
        } else if decoded == "true" {
            serde_json::Value::Bool(true)
        } else if decoded == "false" {
            serde_json::Value::Bool(false)
        } else {
            serde_json::Value::String(decoded)
        };
        map.insert(percent_decode(k), value);
    }
    let value = serde_json::Value::Object(map);
    serde_json::from_value(value)
        .map_err(|e| ApiError::validation(format!("invalid query parameters: {e}"), "query"))
}

/// Minimal percent-decoding for query keys/values.
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = bytes.get(i + 1..i + 3);
                match hex.and_then(|h| {
                    std::str::from_utf8(h)
                        .ok()
                        .and_then(|s| u8::from_str_radix(s, 16).ok())
                }) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A `PATCH`/`DELETE`-shaped empty request body: clients may send
/// `{}` or nothing at all.
#[derive(Debug, Default, Deserialize)]
pub struct EmptyBody {
    /// Placeholder so serde accepts `{}`.
    #[serde(default)]
    #[allow(dead_code)]
    pub _empty: Option<String>,
}

/// Reject non-JSON content types on bodies the endpoint accepts
/// (spec §6.1 `unsupported_media_type`). An absent content type is
/// tolerated (curl without a body, health probes).
pub fn require_json_content_type(headers: &HeaderMap) -> Result<(), ApiError> {
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let mime = content_type.split(';').next().unwrap_or("").trim();
    if mime.eq_ignore_ascii_case("application/json") || mime.is_empty() {
        Ok(())
    } else {
        Err(ApiError::UnsupportedMediaType(format!(
            "expected application/json, got {mime:?}"
        )))
    }
}

/// The JSON shape for user resources (never leaks `password_hash`
/// or `oauth_subject`).
pub fn user_json(user: &User) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "id": user.id,
        "email": user.email,
        "username": user.username,
        "external_id": user.external_id,
        "is_admin": user.is_admin,
        "root_admin": user.root_admin,
    }))
}

/// The permission row's JSON shape for subuser listings.
pub fn subuser_json(row: &Permission, user: &User) -> serde_json::Value {
    serde_json::json!({
        "user_id": row.user_id,
        "instance_id": row.instance_id,
        "email": user.email,
        "username": user.username,
        "grants": row.grants.iter().map(|g| g.as_str()).collect::<Vec<_>>(),
    })
}

/// A request-body extractor that maps axum's `Json` rejections onto
/// the API's structured error vocabulary (§6.1):
///
/// - malformed JSON syntax → `400 bad_request`
/// - well-formed JSON, wrong types → `400 validation_failed`
/// - wrong content type → `415 unsupported_media_type`
/// - body over the limit → `413 payload_too_large`
///
/// Usage: `body: ApiJson<CreateUserRequest>`; the wrapped value is
/// `body.0`.
pub struct ApiJson<T>(pub T);

impl<T, S> axum::extract::FromRequest<S> for ApiJson<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(value) => Ok(Self(value.0)),
            Err(rejection) => {
                let status = rejection.status();
                let message = rejection.body_text();
                let error = match status.as_u16() {
                    415 => ApiError::UnsupportedMediaType(message),
                    413 => ApiError::PayloadTooLarge {
                        limit: crate::config::ApiConfig::default().max_body_bytes,
                    },
                    422 => ApiError::validation(message, "body"),
                    _ => ApiError::bad_request(message),
                };
                Err(error)
            }
        }
    }
}

/// The optional form (`body: Option<ApiJson<T>>`, axum-core's
/// `OptionalFromRequest`): a request with *no* content-type (and
/// therefore no body) extracts as `None`; a request that does carry
/// a body extracts through the structured rejection above. This is
/// what lets endpoints with optional bodies (the §10.3 install
/// mutation, Pterodactyl's own reinstall takes none) stay strict
/// about malformed payloads while tolerating their absence.
impl<T, S> axum_core::extract::OptionalFromRequest<S> for ApiJson<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(
        req: axum::extract::Request,
        state: &S,
    ) -> Result<Option<Self>, Self::Rejection> {
        // No content-type header at all: no body, no extraction.
        if !req.headers().contains_key(axum::http::header::CONTENT_TYPE) {
            return Ok(None);
        }
        // A content-type (of any kind) means a body was sent: run
        // the strict extraction — wrong types surface `415`/`422`
        // exactly as they would for the required form. (The
        // fully-qualified call disambiguates from this trait's own
        // same-named method.)
        <Self as axum::extract::FromRequest<S>>::from_request(req, state)
            .await
            .map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("filter%5Bstate%5D"), "filter[state]");
        // Truncated escape passes through unchanged.
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn grant_parsing_rejects_unknown_names() {
        let ok = parse_grants(&["control.start".to_owned(), "file.read".to_owned()]).unwrap();
        assert_eq!(ok.len(), 2);
        let err = parse_grants(&["control.start".to_owned(), "nope.nope".to_owned()]).unwrap_err();
        assert_eq!(err.code(), "validation_failed");
        assert!(err.to_string().contains("nope.nope"));
    }

    #[test]
    fn uuid_parsing_maps_errors_to_validation() {
        let ok: Result<zim_engine_domain::InstanceId, _> =
            parse_uuid("550e8400-e29b-41d4-a716-446655440000", "id");
        assert!(ok.is_ok());
        let bad: Result<zim_engine_domain::InstanceId, _> = parse_uuid("not-a-uuid", "id");
        assert_eq!(bad.unwrap_err().code(), "validation_failed");
    }

    #[derive(Debug, Default, serde::Deserialize, PartialEq)]
    struct Probe {
        page: Option<u32>,
        sort: Option<String>,
    }

    #[test]
    fn query_params_parse_and_reject() {
        let probe: Probe = query_params("page=2&sort=-created_at&ignored=yes").unwrap();
        assert_eq!(probe.page, Some(2));
        assert_eq!(probe.sort.as_deref(), Some("-created_at"));
        let bad: Result<Probe, _> = query_params("page=two");
        assert_eq!(bad.unwrap_err().code(), "validation_failed");
        let empty: Probe = query_params("").unwrap();
        assert_eq!(empty, Probe::default());
    }
}
