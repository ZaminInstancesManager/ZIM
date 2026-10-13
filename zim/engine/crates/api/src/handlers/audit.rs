//! `GET /api/v1/audit` — the admin-only, cursor-paginated audit query
//! (spec §10.13).

use std::sync::Arc;

use axum::extract::{RawQuery, State};
use axum::{Extension, Json};
use serde::Deserialize;

use crate::error::ApiError;
use crate::handlers::{query_params, require_admin};
use crate::pagination::{cursor_envelope, CursorParams};
use crate::state::{db_or_500, AppState, Caller};

/// Query shape for `GET /audit`.
#[derive(Debug, Default, Deserialize)]
struct AuditQuery {
    cursor: Option<String>,
    limit: Option<u32>,
}

/// `GET /api/v1/audit` — opaque-cursor pagination over the
/// append-only log (§7 + §15).
pub async fn list(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    RawQuery(query): RawQuery,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&caller)?;
    let q: AuditQuery = query_params(query.as_deref().unwrap_or(""))?;
    let params = CursorParams::from_query(q.cursor, q.limit)?;

    // Fetch one extra row to compute has_more without a second query.
    let after = params.after_id;
    let fetch_limit = params.limit as usize + 1;
    let mut records = db_or_500(&state, move |p| {
        p.list_audit(if after == 0 { None } else { Some(after) }, fetch_limit)
    })
    .await?;
    let has_more = records.len() > params.limit as usize;
    if has_more {
        records.truncate(params.limit as usize);
    }
    let page_end = records.last().map(|r| r.id).unwrap_or(params.after_id);
    let data: Vec<_> = records
        .iter()
        .map(|r| serde_json::to_value(r).unwrap_or_default())
        .collect();
    Ok(Json(cursor_envelope(params, page_end, data, has_more)))
}
