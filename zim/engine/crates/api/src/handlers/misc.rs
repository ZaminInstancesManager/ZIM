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
    /// Instance filter — optional since M8: without it, admins get
    /// the global listing and members get a `validation_failed`
    /// (visibility would require a per-user fan-out the §10.8
    /// pagination story covers in the frontend milestone).
    instance_id: Option<String>,
}

/// `GET /api/v1/jobs[?instance_id={id}]` — admin, or a caller with
/// any grant on the instance.
pub async fn list_jobs(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    RawQuery(query): RawQuery,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user = require_user(&caller)?;
    let q: JobsQuery = query_params(query.as_deref().unwrap_or(""))?;
    let instance_id = match q.instance_id.as_deref() {
        Some(raw) => Some(parse_uuid::<zim_engine_domain::InstanceId>(
            raw,
            "instance_id",
        )?),
        None => None,
    };
    match instance_id {
        Some(instance_id) => {
            // Access model mirrors instance reads: admins see the
            // instance's jobs; members need any grant on it.
            let (_instance, _ctx) = load_instance_for_caller(&state, &caller, &instance_id).await?;
            let jobs = db_or_500(&state, {
                let id = instance_id;
                move |p| p.list_jobs_by_instance(&id)
            })
            .await?;
            Ok(Json(serde_json::json!({ "data": jobs })))
        }
        None => {
            // The global listing (M8): admins only. Persistence has
            // no list-all-jobs port; the §10.8 pagination envelope
            // lands with the frontend milestone, so this walks the
            // states that matter.
            if !(user.root_admin || user.is_admin) {
                return Err(ApiError::validation(
                    "global job listing requires an instance_id filter for non-admins",
                    "instance_id",
                ));
            }
            let mut jobs = Vec::new();
            for job_state in [
                zim_engine_domain::status::JobState::Pending,
                zim_engine_domain::status::JobState::InProgress,
                zim_engine_domain::status::JobState::CleanupPending,
                zim_engine_domain::status::JobState::Succeeded,
                zim_engine_domain::status::JobState::Failed,
                zim_engine_domain::status::JobState::Cancelled,
            ] {
                let batch = db_or_500(&state, move |p| p.list_jobs_by_state(job_state)).await?;
                jobs.extend(batch);
            }
            jobs.sort_by_key(|job| job.created_at);
            Ok(Json(serde_json::json!({ "data": jobs })))
        }
    }
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

/// `POST /api/v1/jobs/{id}/cancel` — cancel a durable job (M8).
///
/// `Pending` jobs cancel immediately; `InProgress` jobs get the
/// cooperative signal and finish on the handler's own terms (poll
/// `GET /jobs/{id}` for the terminal state). Terminal and
/// cleaner-owned jobs answer `409 conflict`. Admins, or callers
/// with any grant on the job's instance.
pub async fn cancel_job(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user = require_user(&caller)?;
    let job_id = parse_uuid::<JobId>(&id, "id")?;
    let job = db_or_500(&state, move |p| p.get_job(&job_id))
        .await?
        .ok_or_else(|| ApiError::not_found("job", id.clone()))?;
    // Admins pass; members need visibility on the owning instance.
    if !(user.root_admin || user.is_admin) {
        load_instance_for_caller(&state, &caller, &job.instance_id).await?;
    }

    use zim_engine_jobs::{CancelStatus, JobsError};
    let status = state.jobs.cancel(job.id).await.map_err(|e| match e {
        JobsError::NotFound(id) => ApiError::not_found("job", id),
        JobsError::Uncancellable(id, _) => {
            ApiError::Conflict(format!("job {id} is already in a terminal state"))
        }
        other => ApiError::internal(format!("cannot cancel job: {other}")),
    })?;

    crate::handlers::audit(
        &state,
        Some(user.id.to_string()),
        "job.cancel",
        Some(job.instance_id),
        serde_json::json!({ "job_id": job.id, "status": format!("{status:?}").to_lowercase() }),
    )
    .await;
    Ok(Json(serde_json::json!({
        "job_id": job.id,
        "status": match status {
            CancelStatus::Cancelled => "cancelled",
            CancelStatus::Cancelling => "cancelling",
        },
    })))
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
