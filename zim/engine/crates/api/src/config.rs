//! Runtime configuration knobs for the M5 API surface.
//!
//! Defaults follow `docs/api/ZIM_API_SPEC.md` §9 (rate limits) and
//! §13 (limits) and §12 (idempotency retention). Everything here is
//! in-memory per-process configuration; persisted configuration
//! (engine settings resource) arrives with the frontend package
//! milestone (M11) per the roadmap.

use std::path::PathBuf;
use std::time::Duration;

/// Configuration for the HTTPS loopback API server.
#[derive(Debug, Clone)]
pub struct ApiConfig {
    /// Per-credential request budget (sessions and API keys) within
    /// `rate_limit_window`. Default 600/min per §9.
    pub rate_limit_per_credential: u64,
    /// Sliding-window span for the per-credential limiter.
    pub rate_limit_window: Duration,
    /// Login attempts per IP within `login_window`. Default 10/min
    /// per §9 (brute-force protection on `/auth/login`).
    pub login_rate_limit: u64,
    /// Sliding-window span for the login limiter.
    pub login_window: Duration,
    /// Maximum JSON request body accepted (default 10 MB per §13;
    /// file uploads get their own 5 GB channel per §13 and
    /// [`crate::handlers::files`]).
    pub max_body_bytes: usize,
    /// How long an `Idempotency-Key` response is retained for
    /// replay (default 24 h per §12).
    pub idempotency_retention: Duration,
    /// The value advertised in `X-ZIM-API-Version` (spec §2:
    /// major.minor.patch within v1).
    pub api_version: &'static str,
    /// The session cookie's name (attributes applied by the auth
    /// handlers; scope is the API prefix).
    pub session_cookie: &'static str,
    /// The engine's data root (M28): the parent of the per-instance
    /// data directories the file API operates on. The composition
    /// root sets it from the engine's `--data-dir`; the default
    /// matches `EngineOptions::default` so a bare `ApiConfig`
    /// stays usable in tests that never touch the file family.
    pub data_dir: PathBuf,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            rate_limit_per_credential: 600,
            rate_limit_window: Duration::from_secs(60),
            login_rate_limit: 10,
            login_window: Duration::from_secs(60),
            max_body_bytes: 10 * 1024 * 1024,
            idempotency_retention: Duration::from_secs(24 * 60 * 60),
            api_version: "v1.0.0",
            session_cookie: "zim_session",
            data_dir: PathBuf::from("zim_data"),
        }
    }
}

impl ApiConfig {
    /// The `Path=` scope for the session cookie: the whole versioned
    /// API prefix, so the cookie rides every API request and nothing
    /// else.
    pub fn session_cookie_path(&self) -> &'static str {
        "/api/v1"
    }
}
