//! Shared application state, the caller identity, and the
//! `spawn_blocking` bridge over the sync `Persistence` port.

use std::sync::Arc;
use std::time::Duration;

use zim_engine_application::ports::{Persistence, PersistenceError};
use zim_engine_application::supervision::Supervisor;
use zim_engine_auth::{ApiKeyManager, SessionManager, TabTokenManager};
use zim_engine_backups::BackupService;
use zim_engine_domain::User;
use zim_engine_jobs::JobSystem;
use zim_engine_scheduler::Scheduler;
use zim_engine_transfers::TransferService;

use crate::config::ApiConfig;
use crate::error::ApiError;
use crate::idempotency::IdempotencyStore;
use crate::rate_limit::SlidingWindowLimiter;

/// What kind of credential authenticated the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    /// A session cookie (`zim_session`).
    Session,
    /// An API key bearer token (`zim_...`).
    ApiKey,
}

/// The resolved caller, inserted into request extensions by the auth
/// middleware for every request (anonymous requests carry `None`
/// fields — guards upgrade `None` into `401` as needed).
#[derive(Debug, Clone)]
pub struct Caller {
    /// The authenticated user, when a valid credential was presented.
    pub user: Option<User>,
    /// Which credential kind authenticated the request.
    pub credential: Option<CredentialKind>,
    /// The rate-limit bucket key (`sess:<hash>` / `key:<id>`), when
    /// authenticated.
    pub rate_key: Option<String>,
}

impl Caller {
    /// The anonymous caller (login, health, and unauthenticated
    /// probing).
    pub fn anonymous() -> Self {
        Self {
            user: None,
            credential: None,
            rate_key: None,
        }
    }

    /// The authenticated user or a `401`.
    pub fn require_user(&self) -> Result<&User, ApiError> {
        self.user
            .as_ref()
            .ok_or_else(|| ApiError::unauthorized("this operation requires a credential"))
    }

    /// Whether the caller holds either admin flag.
    pub fn is_admin(&self) -> bool {
        self.user
            .as_ref()
            .is_some_and(|u| u.root_admin || u.is_admin)
    }

    /// The idempotency-store namespace for this caller (anonymous
    /// requests share the `anon` namespace).
    pub fn idempotency_namespace(&self) -> String {
        match &self.rate_key {
            Some(k) => k.clone(),
            None => "anon".to_owned(),
        }
    }
}

/// Everything the handlers need, built once at composition time.
pub struct AppState {
    /// Durable metadata store (sync port; bridge with [`db`]).
    pub persistence: Arc<dyn Persistence>,
    /// The lifecycle supervisor (M6): power operations and observed
    /// state come from here, not from the persisted rows.
    pub supervisor: Arc<Supervisor>,
    /// Live sessions (in-memory by design; see the auth crate docs).
    pub sessions: Arc<SessionManager>,
    /// Issued API keys (in-memory; see the auth crate docs).
    pub api_keys: Arc<ApiKeyManager>,
    /// Per-credential sliding-window limiter (§9).
    pub credential_limiter: SlidingWindowLimiter,
    /// Per-IP limiter for `/auth/login` (§9 brute-force protection).
    pub login_limiter: SlidingWindowLimiter,
    /// `Idempotency-Key` replay store (§12).
    pub idempotency: IdempotencyStore,
    /// Per-tab capability tokens (M7; spec §3's third credential
    /// kind, validated on the WebSocket stream).
    pub tab_tokens: Arc<TabTokenManager>,
    /// The durable-job executor (M8): job creation and cancellation
    /// flow through here; job *reads* go straight to persistence.
    pub jobs: Arc<JobSystem>,
    /// The backup service (M8): backup create/restore/delete/lock
    /// and archive downloads.
    pub backups: Arc<BackupService>,
    /// The transfer service (M8.2): transfer initiation, cancel,
    /// and status reads.
    pub transfers: Arc<TransferService>,
    /// The cron scheduler (M9): schedule firing + run-now.
    pub scheduler: Arc<Scheduler>,
    /// Server configuration.
    pub config: ApiConfig,
}

impl AppState {
    /// Assemble the state from its parts. The two limiters are
    /// constructed from `config`'s budgets.
    ///
    /// (Ten parts is the honest composition surface — a builder
    /// would hide which halves are optional; nothing here is.)
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        persistence: Arc<dyn Persistence>,
        supervisor: Arc<Supervisor>,
        sessions: Arc<SessionManager>,
        api_keys: Arc<ApiKeyManager>,
        tab_tokens: Arc<TabTokenManager>,
        jobs: Arc<JobSystem>,
        backups: Arc<BackupService>,
        transfers: Arc<TransferService>,
        scheduler: Arc<Scheduler>,
        config: ApiConfig,
    ) -> Self {
        Self {
            persistence,
            supervisor,
            sessions,
            api_keys,
            credential_limiter: SlidingWindowLimiter::new(
                config.rate_limit_per_credential,
                config.rate_limit_window,
            ),
            login_limiter: SlidingWindowLimiter::new(config.login_rate_limit, config.login_window),
            idempotency: IdempotencyStore::new(config.idempotency_retention),
            tab_tokens,
            jobs,
            backups,
            transfers,
            scheduler,
            config,
        }
    }
}

/// Bridge a synchronous `Persistence` call onto the async runtime
/// with `spawn_blocking`, per the M3 decision recorded in
/// `crates/application/src/ports.rs`.
///
/// The closure receives the `Arc<dyn Persistence>` so it can make
/// several calls in one blocking hop (read-modify-write sequences
/// stay on one thread).
pub async fn db<F, T>(state: &AppState, f: F) -> Result<T, PersistenceError>
where
    F: FnOnce(&dyn Persistence) -> Result<T, PersistenceError> + Send + 'static,
    T: Send + 'static,
{
    let persistence = Arc::clone(&state.persistence);
    tokio::task::spawn_blocking(move || f(persistence.as_ref()))
        .await
        .map_err(|e| PersistenceError::backend(format!("persistence task join error: {e}")))?
}

/// How long a single `spawn_blocking` persistence hop may take
/// before it is treated as failed (guards against a stuck SQLite
/// lock starving a request forever). Generous: the bundled SQLite
/// holds a 5 s busy timeout, and migrations can take seconds.
pub const DB_HOP_TIMEOUT: Duration = Duration::from_secs(60);

/// `db` plus a timeout; persistence errors surface as
/// `internal_error` (delete paths remap FK failures to `conflict`
/// themselves with [`crate::handlers::conflict_on_backend`]).
pub async fn db_or_500<F, T>(state: &AppState, f: F) -> Result<T, ApiError>
where
    F: FnOnce(&dyn Persistence) -> Result<T, PersistenceError> + Send + 'static,
    T: Send + 'static,
{
    match tokio::time::timeout(DB_HOP_TIMEOUT, db(state, f)).await {
        Ok(inner) => inner.map_err(ApiError::from),
        Err(_) => Err(ApiError::internal("persistence hop timed out")),
    }
}
