//! The SFTP password bridge: engine users over SSH "password" auth.
//!
//! The HTTP login route's semantics, transported: resolve the user
//! (login name may be the username OR the email — SFTP clients
//! habitually take "some user string", and ZIM users know both),
//! verify the Argon2id hash off the async workers, answer a uniform
//! failure that never distinguishes unknown-user from wrong-password.
//! On top of russh's constant-time rejection and per-connection
//! attempt cap, a per-username sliding-window throttle bounds
//! cross-connection brute force (the HTTP route's per-IP limiter
//! equivalent — an SFTP client is one peer, so the username is the
//! honest bucket).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use zim_engine_domain::User;

use crate::SftpShared;

/// How many failed attempts one username may accumulate inside the
/// window before every further attempt is refused fast (no Argon2id
/// work — the throttle itself is the denial).
const ATTEMPT_LIMIT: usize = 5;

/// The sliding window span for [`ATTEMPT_LIMIT`].
const ATTEMPT_WINDOW: Duration = Duration::from_secs(300);

/// One username's recent failures.
#[derive(Default)]
struct AttemptLog {
    failures: Vec<Instant>,
}

/// The per-username failed-login throttle.
#[derive(Default)]
pub(crate) struct AttemptThrottle {
    logs: Mutex<HashMap<String, AttemptLog>>,
}

impl AttemptThrottle {
    /// Whether the username is currently locked out.
    fn is_locked(&self, username: &str) -> bool {
        let mut logs = self.logs.lock().expect("throttle lock");
        match logs.get_mut(username) {
            Some(log) => {
                let now = Instant::now();
                log.failures
                    .retain(|at| now.duration_since(*at) < ATTEMPT_WINDOW);
                log.failures.len() >= ATTEMPT_LIMIT
            }
            None => false,
        }
    }

    /// Record a failure.
    fn record_failure(&self, username: &str) {
        let mut logs = self.logs.lock().expect("throttle lock");
        logs.entry(username.to_owned())
            .or_default()
            .failures
            .push(Instant::now());
    }

    /// Clear the username's log (a successful login forgives the
    /// past — the limit is about sustained guessing, not honest
    /// retries after a typo).
    fn clear(&self, username: &str) {
        let mut logs = self.logs.lock().expect("throttle lock");
        logs.remove(username);
    }
}

/// A resolved, authenticated SFTP login.
#[derive(Debug, Clone)]
pub(crate) struct AuthenticatedUser {
    /// The engine user row.
    pub user: User,
    /// The lowercased login name as presented (audit trail).
    pub login_name: String,
}

/// The uniform failure (no account enumeration — the same answer
/// for unknown user, throttled user, and wrong password).
#[derive(Debug, thiserror::Error)]
#[error("invalid username or password")]
pub(crate) struct InvalidLogin;

/// Resolve + verify one SFTP password login. Blocking-safe (Argon2id
/// + persistence reads); callers bridge with `spawn_blocking`.
pub(crate) fn authenticate(
    shared: &SftpShared,
    login_name: &str,
    password: &str,
) -> Result<AuthenticatedUser, InvalidLogin> {
    let login_name = login_name.trim().to_lowercase();
    if login_name.is_empty() || password.is_empty() {
        return Err(InvalidLogin);
    }
    if shared.throttle.is_locked(&login_name) {
        // Locked-out usernames burn no Argon2id cycles; the answer
        // is byte-for-byte the uniform failure.
        return Err(InvalidLogin);
    }

    // Resolve by email first (the canonical unique key), then by
    // username (SFTP's conventional shape). The user set of a local
    // desktop engine is small; the linear scan for usernames is the
    // honest cost of not adding a second index for a rare lookup.
    let user = shared
        .persistence
        .get_user_by_email(&login_name)
        .ok()
        .flatten()
        .or_else(|| {
            shared
                .persistence
                .list_users()
                .ok()?
                .into_iter()
                .find(|u| u.username.to_lowercase() == login_name)
        });
    let Some(user) = user else {
        shared.throttle.record_failure(&login_name);
        return Err(InvalidLogin);
    };
    let Some(hash) = user.password_hash.clone() else {
        // An OAuth-only account has nothing to verify against.
        shared.throttle.record_failure(&login_name);
        return Err(InvalidLogin);
    };

    let ok = zim_engine_auth::password::verify_password(password, &hash).unwrap_or(false);
    if !ok {
        shared.throttle.record_failure(&login_name);
        return Err(InvalidLogin);
    }

    shared.throttle.clear(&login_name);
    Ok(AuthenticatedUser { user, login_name })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_throttle_locks_after_the_limit_and_forgives_on_success() {
        let throttle = AttemptThrottle::default();
        for _ in 0..ATTEMPT_LIMIT {
            throttle.record_failure("target");
        }
        assert!(throttle.is_locked("target"));
        assert!(!throttle.is_locked("other"));
        // A success clears the log.
        throttle.clear("target");
        assert!(!throttle.is_locked("target"));
    }
}
