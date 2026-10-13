//! Remaining endpoints: health, jobs reads (§10.8), extensions list
//! (§10.12), and the 501 families for eggs (§10.11), frontend
//! packages (§10.14), and workload integrations (§10.16).

use std::sync::Arc;

use axum::extract::{Path, RawQuery, State};
use axum::response::Response;
use axum::{Extension, Json};

use crate::error::ApiError;
use crate::handlers::{load_instance_for_caller, parse_uuid, query_params, require_user};
use crate::state::{db_or_500, AppState, Caller};
use serde::Deserialize;
use zim_engine_domain::JobId;

// ── Health ────────────────────────────────────────────────────────

/// `GET /api/v1/health` — additive endpoint (spec §2 permits
/// additive growth within v1): the browser integration layer's
/// readiness probe. No auth; host validation still applies.
pub async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "zim_engine",
        "api_version": "v1",
    }))
}

// ── Jobs (§10.8) ──────────────────────────────────────────────────

/// Query shape for `GET /jobs`.
#[derive(Debug, Default, Deserialize)]
struct JobsQuery {
    /// Instance filter — job listing is instance-scoped in M5 (the
    /// global listing arrives with the job system in M8, when jobs
    /// exist outside tests).
    instance_id: Option<String>,
}

/// `GET /api/v1/jobs?instance_id={id}` — admin, or a caller with any
/// grant on the instance.
pub async fn list_jobs(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    RawQuery(query): RawQuery,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user = require_user(&caller)?;
    let q: JobsQuery = query_params(query.as_deref().unwrap_or(""))?;
    let instance_id = match q.instance_id.as_deref() {
        None => {
            return Err(ApiError::validation(
                "job listing requires an instance_id filter in M5 (global listing arrives with \
                 the job system in M8)",
                "instance_id",
            ));
        }
        Some(raw) => parse_uuid::<zim_engine_domain::InstanceId>(raw, "instance_id")?,
    };
    // Access model mirrors instance reads: admins see the instance's
    // jobs; members need any grant on it.
    let (_instance, _ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
    let _ = user;
    let jobs = db_or_500(&state, {
        let id = instance_id;
        move |p| p.list_jobs_by_instance(&id)
    })
    .await?;
    Ok(Json(serde_json::json!({ "data": jobs })))
}

/// `GET /api/v1/jobs/{id}` — admin or instance-grant holder.
pub async fn get_job(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user = require_user(&caller)?;
    let job_id = parse_uuid::<JobId>(&id, "id")?;
    let job = db_or_500(&state, move |p| p.get_job(&job_id))
        .await?
        .ok_or_else(|| ApiError::not_found("job", id))?;
    // Admins pass; members need visibility on the owning instance.
    if !(user.root_admin || user.is_admin) {
        load_instance_for_caller(&state, &caller, &job.instance_id).await?;
    }
    Ok(Json(serde_json::to_value(&job).unwrap_or_default()))
}

/// `POST /api/v1/jobs/{id}/cancel` — M8 (job execution).
pub async fn cancel_job_stub() -> Result<Response, ApiError> {
    Err(ApiError::NotImplemented {
        feature: "job cancellation",
        milestone: "M8 (job execution state machine)",
    })
}

// ── Extensions (§10.12) ───────────────────────────────────────────

/// `GET /api/v1/extensions` — list installed extensions (any
/// session; installation itself is admin, M-later).
pub async fn list_extensions(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_user(&caller)?;
    let extensions = db_or_500(&state, |p| p.list_extensions()).await?;
    Ok(Json(serde_json::json!({ "data": extensions })))
}

/// Extension installation family — signature verification pipeline.
pub async fn extension_install_stub() -> Result<Response, ApiError> {
    Err(ApiError::NotImplemented {
        feature: "extension install / uninstall / enable / disable / grant",
        milestone: "post-M11 (extension loader + signature verification)",
    })
}

// ── Eggs (§10.11) ─────────────────────────────────────────────────

/// The egg family — the egg model + importer land in M9.
pub async fn egg_stub() -> Result<Response, ApiError> {
    Err(ApiError::NotImplemented {
        feature: "egg management (list, import, update, delete)",
        milestone: "M9 (egg importer + scheduler)",
    })
}

// ── Frontend packages (§10.14) ────────────────────────────────────

/// The frontend-package family — M11.
pub async fn frontend_stub() -> Result<Response, ApiError> {
    Err(ApiError::NotImplemented {
        feature: "frontend package management (active, upload, smoke-test, reset)",
        milestone: "M11 (frontend loader + default frontend)",
    })
}

// ── Workload integrations (§10.16) ────────────────────────────────

/// The workload-integration admin surface — M16.
pub async fn workload_integration_stub() -> Result<Response, ApiError> {
    Err(ApiError::NotImplemented {
        feature: "workload integration management",
        milestone: "M16 (workload integration SDK)",
    })
}
