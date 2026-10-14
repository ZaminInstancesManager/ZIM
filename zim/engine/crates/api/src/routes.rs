//! Router assembly: every §10 endpoint wired to a handler (real or
//! explicit 501), nested under `/api/v1`, wrapped in the middleware
//! stack, with the request body limit applied.

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::middleware::from_fn_with_state;
use axum::routing::{delete, get, patch, post};
use axum::Router;

use crate::handlers::{audit, auth, instances, misc, nodes, stream, users};
use crate::middleware;
use crate::state::AppState;

/// Build the full API router over `state`.
///
/// Route inventory (§10 of the API spec):
///
/// - Implemented in M5: auth (login/logout/refresh), users + API
///   keys, instances + subusers + allocations + schedules + backup
///   reads + transfer status, nodes + node allocations, jobs reads,
///   audit, extensions list, health (additive).
/// - Implemented in M6: instance lifecycle (start/stop/restart/
///   kill/suspend + live observed state) through the supervision
///   module.
/// - Implemented in M7: the WebSocket event stream (`/stream`,
///   §11), the console buffer read, console input (through to the
///   runtime port — runtimes without an attachment stream still
///   answer 501), and the metrics snapshot.
/// - Declared but 501 `not_implemented`: files (M6 filesystem
///   capability), backup mutations (M8),
///   transfer initiation (M8), schedules run (M9), eggs (M9),
///   frontend packages (M11), workload integrations (M16), OAuth
///   (post-M5).
/// - `/api/v1/workloads` aliases the instance handlers per the
///   AD-19 back-compat rules (§10.17).
pub fn router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        // ── Health (additive, unauthenticated) ────────────────────
        .route("/health", get(misc::health))
        // ── Event stream (§11; M7) ────────────────────────────────
        .route("/stream", get(stream::stream))
        // ── Auth (§10.1) ──────────────────────────────────────────
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/refresh", post(auth::refresh))
        .route("/auth/oauth/{provider}/start", get(auth::oauth_start))
        .route("/auth/oauth/{provider}/callback", get(auth::oauth_callback))
        // ── Users (§10.2) ─────────────────────────────────────────
        .route("/users/me", get(users::me))
        .route("/users", get(users::list).post(users::create))
        .route(
            "/users/{id}",
            get(users::get).patch(users::patch).delete(users::delete),
        )
        .route(
            "/users/{id}/api-keys",
            get(users::list_api_keys).post(users::create_api_key),
        )
        .route(
            "/users/{id}/api-keys/{key_id}",
            delete(users::revoke_api_key),
        )
        // ── Instances (§10.3) ─────────────────────────────────────
        .route("/instances", get(instances::list).post(instances::create))
        .route(
            "/instances/{id}",
            get(instances::get)
                .patch(instances::patch)
                .delete(instances::delete),
        )
        .route("/instances/{id}/state", get(instances::state))
        .route("/instances/{id}/start", post(instances::start))
        .route("/instances/{id}/stop", post(instances::stop))
        .route("/instances/{id}/restart", post(instances::restart))
        .route("/instances/{id}/kill", post(instances::kill))
        .route("/instances/{id}/install", post(instances::install_stub))
        .route("/instances/{id}/suspend", post(instances::suspend))
        .route(
            "/instances/{id}/transfer",
            get(instances::transfer_status)
                .post(instances::transfer_stub)
                .delete(instances::cancel_transfer_stub),
        )
        .route("/instances/{id}/console", get(instances::console))
        .route(
            "/instances/{id}/console/input",
            post(instances::console_input),
        )
        .route("/instances/{id}/metrics", get(instances::metrics))
        .route(
            "/instances/{id}/files",
            get(instances::files_stub).delete(instances::files_stub),
        )
        .route(
            "/instances/{id}/files/content",
            get(instances::files_stub).put(instances::files_stub),
        )
        .route("/instances/{id}/files/create", post(instances::files_stub))
        .route("/instances/{id}/files/archive", post(instances::files_stub))
        .route("/instances/{id}/files/extract", post(instances::files_stub))
        // ── Instance sub-resources (§10.4/6/7/5/9) ────────────────
        .route(
            "/instances/{id}/allocations",
            get(instances::list_allocations).post(instances::assign_allocation),
        )
        .route(
            "/instances/{id}/allocations/{aid}",
            delete(instances::unassign_allocation),
        )
        .route(
            "/instances/{id}/subusers",
            get(instances::list_subusers).post(instances::add_subuser),
        )
        .route(
            "/instances/{id}/subusers/{uid}",
            patch(instances::update_subuser).delete(instances::remove_subuser),
        )
        .route(
            "/instances/{id}/schedules",
            get(instances::list_schedules).post(instances::create_schedule),
        )
        .route(
            "/instances/{id}/schedules/{sid}",
            get(instances::get_schedule)
                .patch(instances::patch_schedule)
                .delete(instances::delete_schedule),
        )
        .route(
            "/instances/{id}/schedules/{sid}/run",
            post(instances::schedule_run_stub),
        )
        .route(
            "/instances/{id}/backups",
            get(instances::list_backups).post(instances::create_backup),
        )
        .route(
            "/instances/{id}/backups/{bid}",
            get(instances::get_backup).delete(instances::delete_backup),
        )
        .route(
            "/instances/{id}/backups/{bid}/restore",
            post(instances::restore_backup),
        )
        .route(
            "/instances/{id}/backups/{bid}/download",
            get(instances::download_backup),
        )
        .route(
            "/instances/{id}/backups/{bid}/lock",
            post(instances::lock_backup).delete(instances::unlock_backup),
        )
        // ── Workloads canonical surface (§10.15) — alias of §10.3 ─
        .route("/workloads", get(instances::list).post(instances::create))
        .route(
            "/workloads/{id}",
            get(instances::get)
                .patch(instances::patch)
                .delete(instances::delete),
        )
        .route("/workloads/{id}/state", get(instances::state))
        .route("/workloads/{id}/start", post(instances::start))
        .route("/workloads/{id}/stop", post(instances::stop))
        .route("/workloads/{id}/restart", post(instances::restart))
        .route("/workloads/{id}/kill", post(instances::kill))
        .route("/workloads/{id}/install", post(instances::install_stub))
        .route(
            "/workloads/{id}/migrate",
            post(instances::workload_kind_stub),
        )
        .route(
            "/workloads/{id}/capabilities",
            get(instances::workload_kind_stub),
        )
        .route(
            "/workloads/{id}/console/input",
            post(instances::console_input),
        )
        .route(
            "/workloads/{id}/network/endpoints",
            get(instances::workload_kind_stub).post(instances::workload_kind_stub),
        )
        .route(
            "/workloads/{id}/network/endpoints/{eid}",
            delete(instances::workload_kind_stub),
        )
        .route(
            "/workloads/{id}/deploy",
            post(instances::workload_kind_stub),
        )
        .route(
            "/workloads/{id}/snapshot",
            post(instances::workload_kind_stub),
        )
        // ── Nodes (§10.10) ────────────────────────────────────────
        .route("/nodes", get(nodes::list).post(nodes::create))
        .route(
            "/nodes/{id}",
            get(nodes::get).patch(nodes::patch).delete(nodes::delete),
        )
        .route("/nodes/{id}/state", get(nodes::state))
        .route(
            "/nodes/{id}/allocations",
            get(nodes::list_allocations).post(nodes::create_allocation),
        )
        // ── Jobs (§10.8) ──────────────────────────────────────────
        .route("/jobs", get(misc::list_jobs))
        .route("/jobs/{id}", get(misc::get_job))
        .route("/jobs/{id}/cancel", post(misc::cancel_job))
        // ── Eggs (§10.11) ─────────────────────────────────────────
        .route("/eggs", get(misc::egg_stub).post(misc::egg_stub))
        .route(
            "/eggs/{id}",
            get(misc::egg_stub)
                .patch(misc::egg_stub)
                .delete(misc::egg_stub),
        )
        // ── Extensions (§10.12) ───────────────────────────────────
        .route(
            "/extensions",
            get(misc::list_extensions).post(misc::extension_install_stub),
        )
        .route("/extensions/{id}", delete(misc::extension_install_stub))
        .route(
            "/extensions/{id}/enable",
            post(misc::extension_install_stub),
        )
        .route(
            "/extensions/{id}/disable",
            post(misc::extension_install_stub),
        )
        .route("/extensions/{id}/grant", post(misc::extension_install_stub))
        // ── Audit (§10.13) ────────────────────────────────────────
        .route("/audit", get(audit::list))
        // ── Frontend packages (§10.14) ────────────────────────────
        .route(
            "/frontend/active",
            get(misc::frontend_stub).post(misc::frontend_stub),
        )
        .route("/frontend/upload", post(misc::frontend_stub))
        .route("/frontend/smoke-test", post(misc::frontend_stub))
        .route("/frontend/reset", post(misc::frontend_stub))
        // ── Workload integrations (§10.16) ────────────────────────
        .route(
            "/workload-integrations",
            get(misc::workload_integration_stub).post(misc::workload_integration_stub),
        )
        .route(
            "/workload-integrations/{id}",
            get(misc::workload_integration_stub).delete(misc::workload_integration_stub),
        )
        .route(
            "/workload-integrations/{id}/enable",
            post(misc::workload_integration_stub),
        )
        .route(
            "/workload-integrations/{id}/disable",
            post(misc::workload_integration_stub),
        );

    // Middleware order = request order: axum's chained `.layer()`
    // makes the LAST call the outermost, so the first request hop is
    // the final `.layer()` below (request-id stamping, so even host
    // rejections carry correlation ids). The stack's behavior is
    // documented in `middleware.rs`'s docs.
    let api: Router = api
        .layer(DefaultBodyLimit::max(state.config.max_body_bytes))
        .layer(from_fn_with_state(
            Arc::clone(&state),
            middleware::idempotency_replay,
        ))
        .layer(from_fn_with_state(
            Arc::clone(&state),
            middleware::credential_rate_limit,
        ))
        .layer(from_fn_with_state(
            Arc::clone(&state),
            middleware::authenticate,
        ))
        .layer(from_fn_with_state(Arc::clone(&state), middleware::version))
        .layer(axum::middleware::from_fn(middleware::host_guard))
        .layer(axum::middleware::from_fn(middleware::request_id))
        .with_state(state);

    // Nest under the versioned prefix (spec §2) and expose.
    Router::new().nest(crate::API_PREFIX, api)
}
