//! Session issuance, validation, and revocation.
//!
//! A session is an opaque 256-bit token handed to the browser inside
//! an `HttpOnly` / `Secure` / `SameSite=Strict` cookie (attributes
//! applied by the M5 HTTP layer). Only the token's SHA-256 is kept
//! in memory. Sessions carry both an absolute lifetime and an idle
//! timeout: either expiring invalidates the session. Validation
//! slides the idle window (updates `last_seen_at`).
//!
//! Sessions are in-memory by design (see the crate docs): an engine
//! restart invalidates all sessions, which for a loopback-scoped API
//! just means logging in again, and avoids persisting another
//! secret.

use crate::token::{generate_token, hash_token};
use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
use std::sync::RwLock;
use thiserror::Error;
use zim_engine_domain::UserId;

/// Session lifetime configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionPolicy {
    /// Absolute session lifetime (default 24 h). A session dies at
    /// `issued_at + lifetime` no matter how active it is.
    pub lifetime: chrono::Duration,
    /// Idle timeout (default 30 min). A session dies if
    /// `now - last_seen_at` exceeds this.
    pub idle_timeout: chrono::Duration,
}

impl Default for SessionPolicy {
    fn default() -> Self {
        Self {
            lifetime: Duration::hours(24),
            idle_timeout: Duration::minutes(30),
        }
    }
}

/// A live session, keyed by the SHA-256 of its token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// SHA-256 hex of the session token (the at-rest key).
    pub token_hash: String,
    /// The authenticated user.
    pub user_id: UserId,
    /// When the session was issued.
    pub created_at: DateTime<Utc>,
    /// Absolute expiry (`created_at + lifetime`).
    pub expires_at: DateTime<Utc>,
    /// Last successful validation (sliding idle window).
    pub last_seen_at: DateTime<Utc>,
}

/// A freshly issued session: the plaintext token (shown exactly
/// once — it is not stored) plus the session record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedSession {
    /// The opaque token to set as the session cookie value.
    pub token: String,
    /// The stored session record (carries `token_hash`, not the
    /// token).
    pub session: Session,
}

/// Errors from session validation and revocation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SessionError {
    /// The token does not name a session (unknown, already revoked,
    /// or expired-and-pruned). Deliberately undistinguished so the
    /// API layer answers a uniform `401 unauthorized`.
    #[error("invalid session")]
    Invalid,
    /// The session's absolute lifetime elapsed.
    #[error("session expired")]
    Expired,
    /// The session's idle timeout elapsed.
    #[error("session idle-expired")]
    IdleExpired,
}

/// In-memory session manager.
pub struct SessionManager {
    policy: SessionPolicy,
    sessions: RwLock<HashMap<String, Session>>,
}

impl SessionManager {
    /// A manager with the given policy and no sessions.
    pub fn new(policy: SessionPolicy) -> Self {
        Self {
            policy,
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// The manager's policy.
    pub fn policy(&self) -> &SessionPolicy {
        &self.policy
    }

    /// Issues a new session for `user_id`.
    ///
    /// The plaintext token appears only in the returned
    /// [`IssuedSession`]; the map stores its SHA-256.
    pub fn issue(&self, user_id: &UserId, now: DateTime<Utc>) -> IssuedSession {
        let token = generate_token(32);
        let session = Session {
            token_hash: hash_token(&token),
            user_id: *user_id,
            created_at: now,
            expires_at: now + self.policy.lifetime,
            last_seen_at: now,
        };
        self.sessions
            .write()
            .expect("session map lock poisoned")
            .insert(session.token_hash.clone(), session.clone());
        IssuedSession { token, session }
    }

    /// Validates a session token and slides its idle window.
    ///
    /// `Ok(session)` — the token names a live session;
    /// `Err(Expired | IdleExpired | Invalid)` — see
    /// [`SessionError`] for the distinction. Expired sessions are
    /// removed as a side effect.
    pub fn validate(&self, token: &str, now: DateTime<Utc>) -> Result<Session, SessionError> {
        let token_hash = hash_token(token);
        let mut sessions = self.sessions.write().expect("session map lock poisoned");

        let session = sessions.get_mut(&token_hash).ok_or(SessionError::Invalid)?;
        if now > session.expires_at {
            sessions.remove(&token_hash);
            return Err(SessionError::Expired);
        }
        let idle_deadline = session.last_seen_at + self.policy.idle_timeout;
        if now > idle_deadline {
            sessions.remove(&token_hash);
            return Err(SessionError::IdleExpired);
        }
        session.last_seen_at = now;
        Ok(session.clone())
    }

    /// Revokes the session named by `token` (logout).
    pub fn revoke(&self, token: &str, _now: DateTime<Utc>) -> Result<(), SessionError> {
        let token_hash = hash_token(token);
        self.sessions
            .write()
            .expect("session map lock poisoned")
            .remove(&token_hash)
            .map(|_| ())
            .ok_or(SessionError::Invalid)
    }

    /// Revokes every session for `user_id` (password change, account
    /// disable). Returns how many sessions were revoked.
    pub fn revoke_all_for_user(&self, user_id: &UserId) -> usize {
        let mut sessions = self.sessions.write().expect("session map lock poisoned");
        let before = sessions.len();
        sessions.retain(|_, session| session.user_id != *user_id);
        before - sessions.len()
    }

    /// Removes sessions whose absolute lifetime has elapsed.
    /// Returns how many were pruned. The API layer calls this
    /// periodically (or on validation, which prunes lazily).
    ///
    /// A session at exactly `expires_at` is still live (consistent
    /// with [`SessionManager::validate`]'s strict `now > expires_at`
    /// expiry check).
    pub fn prune(&self, now: DateTime<Utc>) -> usize {
        let mut sessions = self.sessions.write().expect("session map lock poisoned");
        let before = sessions.len();
        sessions.retain(|_, session| session.expires_at >= now);
        before - sessions.len()
    }

    /// Number of live (not yet pruned) sessions.
    pub fn len(&self) -> usize {
        self.sessions
            .read()
            .expect("session map lock poisoned")
            .len()
    }

    /// Whether the manager holds no sessions.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_now() -> DateTime<Utc> {
        // A fixed, non-trivial epoch so arithmetic is easy to eyeball.
        DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn policy_defaults() {
        let policy = SessionPolicy::default();
        assert_eq!(policy.lifetime, Duration::hours(24));
        assert_eq!(policy.idle_timeout, Duration::minutes(30));
    }

    #[test]
    fn issue_then_validate_round_trips() {
        let manager = SessionManager::new(SessionPolicy::default());
        let user = UserId::new();
        let now = fixed_now();

        let issued = manager.issue(&user, now);
        assert_eq!(issued.token.len(), 43);
        assert_ne!(issued.token, issued.session.token_hash);

        let validated = manager.validate(&issued.token, now).unwrap();
        assert_eq!(validated.user_id, user);
        assert_eq!(validated.expires_at, now + Duration::hours(24));
    }

    #[test]
    fn validate_slides_the_idle_window() {
        let manager = SessionManager::new(SessionPolicy {
            idle_timeout: Duration::minutes(5),
            ..SessionPolicy::default()
        });
        let issued = manager.issue(&UserId::new(), fixed_now());

        // 4 minutes pass: still valid, window slides.
        assert!(manager
            .validate(&issued.token, fixed_now() + Duration::minutes(4))
            .is_ok());
        // 4 more minutes since the slide (8 total, but only 4 since
        // last_seen): still valid.
        assert!(manager
            .validate(&issued.token, fixed_now() + Duration::minutes(8))
            .is_ok());
        // 6 idle minutes after the last slide: idle-expired (and
        // removed).
        assert_eq!(
            manager
                .validate(&issued.token, fixed_now() + Duration::minutes(14))
                .unwrap_err(),
            SessionError::IdleExpired
        );
        // Now unknown:
        assert_eq!(
            manager
                .validate(&issued.token, fixed_now() + Duration::minutes(15))
                .unwrap_err(),
            SessionError::Invalid
        );
    }

    #[test]
    fn absolute_lifetime_wins_over_activity() {
        let manager = SessionManager::new(SessionPolicy {
            lifetime: Duration::hours(1),
            idle_timeout: Duration::hours(24),
        });
        let issued = manager.issue(&UserId::new(), fixed_now());
        // Active every 10 minutes... but the absolute cap is 1 h.
        for minute in 0..6 {
            assert!(
                manager
                    .validate(&issued.token, fixed_now() + Duration::minutes(minute * 10))
                    .is_ok(),
                "should be valid at minute {}",
                minute * 10
            );
        }
        assert_eq!(
            manager
                .validate(&issued.token, fixed_now() + Duration::minutes(61))
                .unwrap_err(),
            SessionError::Expired
        );
    }

    #[test]
    fn revoke_kills_the_session() {
        let manager = SessionManager::new(SessionPolicy::default());
        let issued = manager.issue(&UserId::new(), fixed_now());
        assert_eq!(manager.len(), 1);
        manager.revoke(&issued.token, fixed_now()).unwrap();
        assert_eq!(manager.len(), 0);
        assert_eq!(
            manager.validate(&issued.token, fixed_now()).unwrap_err(),
            SessionError::Invalid
        );
    }

    #[test]
    fn revoke_all_for_user_is_scoped() {
        let manager = SessionManager::new(SessionPolicy::default());
        let alice = UserId::new();
        let bob = UserId::new();
        let now = fixed_now();
        manager.issue(&alice, now);
        manager.issue(&alice, now);
        manager.issue(&bob, now);
        assert_eq!(manager.revoke_all_for_user(&alice), 2);
        assert_eq!(manager.len(), 1);
    }

    #[test]
    fn prune_removes_only_expired() {
        let manager = SessionManager::new(SessionPolicy {
            lifetime: Duration::hours(1),
            ..SessionPolicy::default()
        });
        let now = fixed_now();
        let old = manager.issue(&UserId::new(), now);
        manager.issue(&UserId::new(), now + Duration::minutes(30));
        assert_eq!(manager.len(), 2);

        // At now+2h the first session is past its lifetime; the
        // second (issued 30 min later, lifetime 1 h) expired too.
        // Use a time where exactly one is expired:
        assert_eq!(manager.prune(now + Duration::minutes(90)), 1);
        assert_eq!(manager.len(), 1);
        assert_eq!(
            manager
                .validate(&old.token, now + Duration::minutes(90))
                .unwrap_err(),
            SessionError::Invalid
        );
    }

    #[test]
    fn tokens_cannot_be_validated_by_hash() {
        // The stored form (hash) is not a credential: presenting it
        // as a token hashes to a different value and fails.
        let manager = SessionManager::new(SessionPolicy::default());
        let issued = manager.issue(&UserId::new(), fixed_now());
        assert_eq!(
            manager
                .validate(&issued.session.token_hash, fixed_now())
                .unwrap_err(),
            SessionError::Invalid
        );
    }
}
