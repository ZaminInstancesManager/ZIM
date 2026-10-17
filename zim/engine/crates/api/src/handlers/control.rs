//! `POST /api/v1/engine/control` — the local control plane's
//! surface (M27; see `crate::control` for the design and
//! `docs/architecture/UPDATES.md` §3 for the Windows-update
//! motivation).
//!
//! One action exists: `shutdown`, which fires the same graceful
//! drain SIGTERM does. The endpoint deliberately does NOT accept
//! user credentials as an alternative to the engine token: mixing
//! "a logged-in admin may stop the whole engine" into the user
//! authz model would let any remote-ish session-bearing client
//! (a browser tab pointed at the loopback origin) take the engine
//! down, while the token path is provably machine-local.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use crate::control::TOKEN_HEADER;
use crate::state::AppState;

/// The request body: `{"action":"shutdown"}`.
#[derive(Debug, Deserialize)]
pub struct ControlRequest {
    /// The requested action. Only `shutdown` exists.
    pub action: String,
}

/// `POST /api/v1/engine/control` — token-gated, loopback-guarded
/// (the host middleware), unauthenticated-by-design.
///
/// - missing header → `401` with the structured error body
/// - wrong token → `401`
/// - unknown action → `422` (`validation_failed`)
/// - `shutdown` → `202 {"status":"draining"}`, the drain proceeds
///   asynchronously exactly as a SIGTERM's would (the response is
///   sent before the listener stops accepting — the caller's own
///   probe loop watches for the process to exit).
pub async fn control(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Option<Json<ControlRequest>>,
) -> Response {
    // The token check first: an unauthenticated caller learns
    // nothing about which actions exist from a 401's shape.
    let presented = headers
        .get(header::HeaderName::from_static(TOKEN_HEADER))
        .and_then(|value| value.to_str().ok());
    let presented = match presented {
        Some(raw) => raw,
        None => {
            return refusal(
                StatusCode::UNAUTHORIZED,
                "the engine-control endpoint requires the X-ZIM-Engine-Token header",
            );
        }
    };
    if !state.control.verify(presented) {
        return refusal(
            StatusCode::UNAUTHORIZED,
            "the presented engine-control token is not valid",
        );
    }

    let request = match body {
        Some(Json(request)) => request,
        None => {
            return refusal(
                StatusCode::UNAUTHORIZED,
                "the engine-control request body is malformed",
            );
        }
    };

    match request.action.as_str() {
        "shutdown" => {
            // The audit trail: actor System — the caller is the
            // machine-local operator context (the shell), not a
            // user; the act itself is recorded like any other
            // privileged operation.
            let record = zim_engine_domain::AuditRecord {
                id: 0,
                timestamp: chrono::Utc::now(),
                actor: zim_engine_domain::AuditActor::System,
                actor_id: None,
                action: "engine.control.shutdown".to_owned(),
                instance_id: None,
                metadata: serde_json::json!({
                    "via": "engine_control",
                    "pid": std::process::id(),
                }),
                ip_address: None,
                user_agent: headers
                    .get(header::USER_AGENT)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned),
            };
            let persistence = Arc::clone(&state.persistence);
            let action = record.action.clone();
            let append = tokio::task::spawn_blocking(move || persistence.append_audit(&record));
            if let Ok(Err(e)) = append.await {
                eprintln!("zim_engine_api: audit append failed for {action:?}: {e}");
            }

            state.control.shutdown_handle().trigger();
            (
                StatusCode::ACCEPTED,
                Json(serde_json::json!({
                    "status": "draining",
                    "grace": "jobs quiesce, supervision stops, connections drain; the process exits when the drain completes",
                })),
            )
                .into_response()
        }
        other => refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("unknown engine-control action {other:?} (only \"shutdown\" exists)"),
        ),
    }
}

/// A structured refusal (the API's error envelope shape, kept
/// consistent with `ApiError`'s bodies for client-side parsing).
fn refusal(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({
            "error": {
                "code": if status == StatusCode::UNAUTHORIZED { "unauthorized" } else { "validation_failed" },
                "message": message,
            }
        })),
    )
        .into_response()
}
