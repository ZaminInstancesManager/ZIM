//! Router assembly: every §10 endpoint wired to a handler (real or
//! explicit 501), nested under `/api/v1`, wrapped in the middleware
//! stack, with the request body limit applied — plus (M11) the
//! `/zim/` page surface over the active frontend package.

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::middleware::from_fn_with_state;
use axum::routing::{delete, get, patch, post};
use axum::Router;

use crate::handlers::{
    audit, auth, control, eggs, extensions, files, frontend, instances, misc, nodes, pages,
    recents, stream, themes, users,
};
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
/// - Implemented in M7.1 (streaming) + M8.1 (jobs + backups): the
///   WebSocket event stream (`/stream`, §11), the console buffer
///   read + input, the metrics snapshot, the durable-job endpoints,
///   and the §10.5 backup mutation family (create / delete /
///   restore / download / lock).
/// - Implemented in M8.2: the §10.9 transfer mutation family
///   (initiate / cancel) over the two-phase-commit job.
/// - Implemented in M8.3: the §10.3 install mutation
///   (`POST /instances/{id}/install`) over the constrained-container
///   install job.
/// - Implemented in M11: the §10.14 frontend-package family
///   (active / activate / upload / smoke-test / reset) over the
///   frontend loader, plus the `/zim/` page surface (the active
///   package's pages and assets, with the §3.8 CSP).
/// - Implemented in M28: the §10.3 file family (list / content
///   get+put / create / delete / rename / archive / extract) over
///   the chrooted files crate, with the route-level §13 upload
///   channel (5 GB) inside the router-wide JSON limit.
/// - Declared but 501 `not_implemented`: workload integrations
///   (M16's registry surface), OAuth (post-M5).
/// - `/api/v1/workloads` aliases the instance handlers per the
///   AD-19 back-compat rules (§10.17).
pub fn router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        // ── Health (additive, unauthenticated) ────────────────────
        .route("/health", get(misc::health))
        // ── Local engine control (M27; UPDATES.md §3) ─────────────
        // Token-gated, NOT user-credential-gated (the machine-local
        // operator context the desktop shell occupies; see
        // handlers::control for why the two models stay separate).
        .route("/engine/control", post(control::control))
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
        .route("/me/recents", get(recents::list).post(recents::record))
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
        .route("/instances/{id}/install", post(instances::initiate_install))
        .route("/instances/{id}/suspend", post(instances::suspend))
        .route(
            "/instances/{id}/transfer",
            get(instances::transfer_status)
                .post(instances::initiate_transfer)
                .delete(instances::cancel_transfer),
        )
        .route("/instances/{id}/console", get(instances::console))
        .route(
            "/instances/{id}/console/input",
            post(instances::console_input),
        )
        .route("/instances/{id}/metrics", get(instances::metrics))
        // ── Files (§10.3, M28) ───────────────────────────────
        // The content pair carries the §13 upload channel (5 GB)
        // layered inside the router-wide 10 MB JSON limit — the
        // route-level DefaultBodyLimit is the innermost and wins.
        .route(
            "/instances/{id}/files",
            get(files::list).delete(files::delete),
        )
        .route(
            "/instances/{id}/files/content",
            get(files::download)
                .put(files::upload)
                .layer(DefaultBodyLimit::max(files::MAX_UPLOAD_BYTES)),
        )
        .route("/instances/{id}/files/create", post(files::create))
        .route("/instances/{id}/files/rename", post(files::rename))
        .route("/instances/{id}/files/archive", post(files::archive))
        .route("/instances/{id}/files/extract", post(files::extract))
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
            post(instances::schedule_run),
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
        .route("/workloads/{id}/install", post(instances::initiate_install))
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
        .route("/eggs", get(eggs::list_eggs).post(eggs::import_egg))
        .route(
            "/eggs/{id}",
            get(eggs::get_egg)
                .patch(eggs::patch_egg)
                .delete(eggs::delete_egg),
        )
        // ── Extensions (§10.12, M24) ─────────────────────────
        .route(
            "/extensions",
            get(extensions::list).post(extensions::install),
        )
        .route("/extensions/{id}", delete(extensions::uninstall))
        .route("/extensions/{id}/enable", post(extensions::enable))
        .route("/extensions/{id}/disable", post(extensions::disable))
        .route("/extensions/{id}/grant", post(extensions::grant))
        // ── Themes (M23's deferred upload surface, M24) ────────
        .route("/themes", get(themes::list).post(themes::upload))
        .route("/themes/{id}", delete(themes::delete))
        // ── Audit (§10.13) ────────────────────────────────────────
        .route("/audit", get(audit::list))
        // ── Frontend packages (§10.14) ────────────────────────────
        .route(
            "/frontend/active",
            get(frontend::active).post(frontend::set_active),
        )
        .route("/frontend/upload", post(frontend::upload))
        .route("/frontend/smoke-test", post(frontend::smoke_test))
        .route("/frontend/reset", post(frontend::reset))
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
        .layer(from_fn_with_state(
            Arc::clone(&state),
            middleware::addon_attribution,
        ))
        .layer(from_fn_with_state(Arc::clone(&state), middleware::version))
        .layer(axum::middleware::from_fn(middleware::host_guard))
        .layer(axum::middleware::from_fn(middleware::request_id))
        .with_state(state.clone());

    // Nest under the versioned prefix (spec §2) and expose.
    Router::new()
        .nest(crate::API_PREFIX, api)
        .merge(pages_router(state))
}

/// The `/zim/` page surface (M11): the active frontend package's
/// pages and assets, served under the loopback origin so the
/// default frontend is reachable in any browser (the `zim://`
/// scheme itself arrives with the Chromium integration, M13).
///
/// The stack is deliberately thinner than the API's: host guard
/// and request ids apply, but the pages are unauthenticated static
/// shells (all data flows through the credential-checked API), so
/// the auth/version/rate-limit/idempotency middlewares do not.
fn pages_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/zim", get(pages::root))
        .route("/zim/", get(pages::root))
        .route("/zim/{*path}", get(pages::serve))
        .layer(axum::middleware::from_fn(middleware::host_guard))
        .layer(axum::middleware::from_fn(middleware::request_id))
        .with_state(state)
}
