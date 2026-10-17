//! HTTP + WebSocket handlers for the ZIM engine API.
//!
//! Thin layer that translates HTTPS+WSS requests to application service
//! calls and enforces capability checks at the API boundary (per
//! docs/architecture/ZIM_ARCHITECTURE.md §4 invariant 15). The
//! canonical API surface is documented in docs/api/ZIM_API_SPEC.md.
//!
//! # Architecture boundary
//!
//! This crate's role, dependencies, and permitted callers are documented
//! in `docs/architecture/RUST_ENGINE.md` (and the expanded-scope
//! `docs/architecture/WORKLOAD_ABSTRACTION.md` where applicable).
//! The permitted dependency direction is enforced at compile time by
//! the workspace crate graph.
//!
//! # Milestone scope (M5)
//!
//! This crate implements milestone M5 of
//! `docs/architecture/IMPLEMENTATION_ROADMAP.md` — the HTTPS API
//! surface for read operations and basic write operations (no
//! lifecycle, no streaming, no job execution yet):
//!
//! - Loopback-only HTTPS listener with a self-signed certificate
//!   (`tls`), `Host`-header validation against DNS rebinding
//!   (`middleware::host_guard`), and `X-ZIM-API-Version`
//!   negotiation (`middleware::version`).
//! - Structured error bodies with `request_id` correlation
//!   (`error`, `middleware::request_id`) per spec §6.
//! - Authentication: login/logout/refresh with `HttpOnly`/`Secure`/
//!   `SameSite=Strict` cookies, API-key bearers (`handlers::auth`,
//!   `middleware::authenticate`).
//! - Authorization: per-endpoint capability checks through the M4
//!   `zim_engine_authz` primitives (`handlers` guards).
//! - Page-based and cursor-based pagination (`pagination`),
//!   `ETag`/`If-Match` concurrency control (`etag`), 24-hour
//!   `Idempotency-Key` replay (`idempotency`), and sliding-window
//!   rate limits (`rate_limit`) per spec §7–§12.
//! - Handlers for the §10 endpoints whose dependencies exist
//!   (auth, users + API keys, instances + subusers + allocations +
//!   schedules, nodes, jobs reads, audit, extensions) plus explicit
//!   `501 not_implemented` handlers for endpoints declared in the
//!   spec but scheduled for later milestones (lifecycle, files,
//!   console, backups creation, transfers initiation, eggs,
//!   frontend packages, workload integrations).
//!
//! The WebSocket event stream (`/api/v1/stream`, §11) shipped with
//! M7: `handlers::stream` upgrades authorized subscribers onto the
//! streaming hub, with replay-from-checkpoint and the §11.2
//! backpressure contract enforced crate-side.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod config;
pub mod control;
pub mod error;
pub mod etag;
pub mod handlers;
pub mod idempotency;
pub mod middleware;
pub mod pagination;
pub mod rate_limit;
pub mod routes;
pub mod state;
pub mod tls;

pub use config::ApiConfig;
pub use error::ApiError;
pub use routes::router;
pub use state::{AppState, Caller, CredentialKind};

/// The canonical crate name, useful for runtime identification in
/// logs and crash reports.
pub const CRATE_NAME: &str = "zim_engine_api";

/// The URL prefix every M5 endpoint lives under (spec §2).
pub const API_PREFIX: &str = "/api/v1";

#[cfg(test)]
mod tests {
    //! Cross-module smoke tests; per-module behavior is tested in the
    //! respective modules and in the socket-level integration tests
    //! (`tests/`).

    use super::*;

    #[test]
    fn crate_name_is_set() {
        assert_eq!(CRATE_NAME, "zim_engine_api");
    }
}
