//! Local users + OAuth.
//!
//! User accounts with Argon2id password hashing. Session management
//! (HttpOnly / Secure / SameSite=Strict cookies — the cookie
//! attributes themselves are applied by the HTTP layer in M5; this
//! crate issues and validates the underlying opaque tokens). OAuth
//! callback support (Discord first per
//! `docs/architecture/DECISIONS.md` §Q8 — the HTTP callback flow is
//! M5; the `oauth_subject` account linkage is carried by the domain
//! `User`). API key issuance and revocation per
//! `docs/api/ZIM_API_SPEC.md` §10.2.
//!
//! # Milestone scope (M4)
//!
//! This crate ships the credential core of milestone M4
//! (`docs/architecture/IMPLEMENTATION_ROADMAP.md` §M4):
//!
//! - `password`: Argon2id hash/verify with a password policy
//!   (length bounds; hashing parameters fixed at the OWASP-recommended
//!   Argon2id profile — 19 MiB, t=2, p=1).
//! - `token`: opaque token generation (base64url, OS entropy) and
//!   SHA-256 at-rest hashing. Tokens are stored hashed; the plaintext
//!   token is shown to the caller exactly once at issuance.
//! - `session`: in-memory session manager with lifetime + idle
//!   timeouts, revocation, and pruning. Sessions intentionally do
//!   not survive an engine restart (the cookie just goes stale; the
//!   user logs in again) — acceptable for a loopback-scoped engine
//!   API and one less persisted secret.
//! - `api_key`: API key manager (`zim_<key_id>_<secret>` format,
//!   scoped to a `PermissionSet`, optional expiry, revocation, and
//!   last-use tracking).
//!
//! The HTTP middleware (cookie attributes, `Authorization: Bearer`
//! parsing, and the `401`/`403` mapping) lands with the API surface
//! in M5.
//!
//! # Architecture boundary
//!
//! This crate's role, dependencies, and permitted callers are documented
//! in `docs/architecture/RUST_ENGINE.md` (and the expanded-scope
//! `docs/architecture/WORKLOAD_ABSTRACTION.md` where applicable).
//! The permitted dependency direction is enforced at compile time by
//! the workspace crate graph.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod api_key;
pub mod password;
pub mod session;
pub mod token;

pub use api_key::{ApiKeyError, ApiKeyManager, ApiKeyRecord, IssuedApiKey};
pub use password::{PasswordError, PasswordPolicy};
pub use session::{IssuedSession, Session, SessionError, SessionManager, SessionPolicy};

/// The canonical crate name, useful for runtime identification in
/// logs and crash reports.
pub const CRATE_NAME: &str = "zim_engine_auth";

#[cfg(test)]
mod tests {
    //! Cross-module smoke tests; module behavior is tested in the
    //! respective modules.

    use super::*;
    use zim_engine_domain::UserId;

    #[test]
    fn crate_name_is_set() {
        assert_eq!(CRATE_NAME, "zim_engine_auth");
    }

    #[test]
    fn password_login_and_session_flow() {
        // The end-to-end local-account flow minus the HTTP layer:
        // set a password, verify it, issue a session, validate it,
        // revoke it.
        let policy = PasswordPolicy::default();
        let hash = password::hash_password(&policy, "correct horse battery staple").unwrap();
        assert!(password::verify_password("correct horse battery staple", &hash).unwrap());
        assert!(!password::verify_password("wrong password", &hash).unwrap());

        let manager = SessionManager::new(SessionPolicy::default());
        let user_id = UserId::new();
        let issued = manager.issue(&user_id, chrono::Utc::now());
        let validated = manager.validate(&issued.token, chrono::Utc::now()).unwrap();
        assert_eq!(validated.user_id, user_id);
        manager.revoke(&issued.token, chrono::Utc::now()).unwrap();
        assert!(manager.validate(&issued.token, chrono::Utc::now()).is_err());
    }
}
