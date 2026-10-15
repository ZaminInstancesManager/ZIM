//! `/api/v1/me/recents` — the caller's recent servers (M23).
//!
//! Engine-backed quick-access state for the new-tab page (§9 /
//! §11 of the integration architecture): the frontend records an
//! open when a pane adopts a server view, and reads the ranked
//! rows back for its tiles. User-scoped, never a cross-user
//! observable, and deliberately **not audited** — an "open" is
//! high-frequency nicety state, not a lifecycle mutation; the
//! audit log stays for actions that move engine authority.

use std::sync::Arc;

use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::{Extension, Json};
use chrono::Utc;
use serde::Deserialize;

use crate::error::ApiError;
use crate::handlers::{parse_uuid, query_params, require_json_content_type, require_user, ApiJson};
use crate::state::{db_or_500, AppState, Caller};
use zim_engine_domain::{InstanceId, RecentServer};

/// Query shape for `GET /me/recents`.
#[derive(Debug, Default, Deserialize)]
struct ListRecentsQuery {
    limit: Option<u32>,
}

/// `GET /api/v1/me/recents?limit=` — the caller's recent servers,
/// most-recently-opened first. `limit` defaults to 8 and is clamped
/// to `[1, 32]` (the store keeps at most 32 per user anyway).
pub async fn list(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    RawQuery(query): RawQuery,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user = require_user(&caller)?;
    let q: ListRecentsQuery = query_params(query.as_deref().unwrap_or(""))?;
    let limit = q.limit.unwrap_or(8).clamp(1, 32);

    let user_id = user.id;
    let rows = db_or_500(&state, move |p| p.list_recent_servers(&user_id, limit)).await?;
    Ok(Json(
        serde_json::json!({ "data": rows.iter().map(recent_json).collect::<Vec<_>>() }),
    ))
}

/// `POST /api/v1/me/recents` — record that the caller opened a
/// server view. The instance must exist (404 otherwise): recents
/// point at real servers, and the NTP merges them with the visible
/// instance list client-side.
#[derive(Debug, Deserialize)]
pub struct RecordRecentRequest {
    /// The opened instance's id.
    pub instance_id: String,
}

/// `POST /api/v1/me/recents`.
pub async fn record(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<RecordRecentRequest>,
) -> Result<StatusCode, ApiError> {
    let user = require_user(&caller)?;
    require_json_content_type(&headers)?;
    let instance = parse_uuid::<InstanceId>(&request.instance_id, "instance_id")?;

    let probe = instance;
    let exists = db_or_500(&state, move |p| p.get_instance(&probe))
        .await?
        .is_some();
    if !exists {
        return Err(ApiError::not_found("instance", request.instance_id));
    }

    let user_id = user.id;
    let now = Utc::now();
    db_or_500(&state, move |p| {
        p.record_recent_server(&user_id, &instance, now)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The wire shape of one recent-servers row.
fn recent_json(row: &RecentServer) -> serde_json::Value {
    serde_json::json!({
        "instance_id": row.instance_id.to_string(),
        "last_opened_at": row.last_opened_at.to_rfc3339(),
        "open_count": row.open_count,
    })
}
