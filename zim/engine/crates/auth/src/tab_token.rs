//! Tab capability tokens (ZIM_API_SPEC.md §3's third credential
//! kind, engine-side support).
//!
//! A tab token is issued by the engine to one browser tab for one
//! instance, scoped to that tab's permission set, short-lived, and
//! revocable when the tab closes. The browser-integration layer
//! (the Chromium side) requests issuance when a tab navigates to a
//! `zim://servers/{id}/…` URL and refreshes it on tab focus; M7
//! ships the engine-side half — the manager and the
//! `X-ZIM-Tab-Token` validation path on the WebSocket stream —
//! while the issuance request surface arrives with the browser
//! integration milestone.
//!
//! Like sessions, tab tokens are in-memory by design: they are
//! strictly more short-lived than sessions, so surviving an engine
//! restart would be pointless even if it were possible. At rest
//! they are SHA-256 hashes of the opaque token (see `token`).

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rand_core::{OsRng, RngCore};
use thiserror::Error;
use uuid::Uuid;
use zim_engine_domain::{InstanceId, PermissionSet, UserId};

use crate::token::{generate_token, hash_token};

/// Tunables of the tab-token manager.
#[derive(Debug, Clone)]
pub struct TabTokenPolicy {
    /// How long a tab token lives before it must be refreshed
    /// (spec §3: "short-lived"; the integration layer refreshes on
    /// tab focus). Default 5 minutes.
    pub ttl: Duration,
}

impl Default for TabTokenPolicy {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(5 * 60),
        }
    }
}

/// The validated view of a tab token (what `validate` returns).
#[derive(Debug, Clone)]
pub struct TabTokenView {
    /// The token's user.
    pub user_id: UserId,
    /// The instance the token is scoped to.
    pub instance_id: InstanceId,
    /// The tab the token belongs to.
    pub tab_id: Uuid,
    /// The permission set the tab holds for the instance.
    pub grants: PermissionSet,
    /// When the token expires (the caller should refresh before
    /// then).
    pub expires_at: DateTime<Utc>,
}

/// A freshly issued tab token (the plaintext is shown exactly
/// once).
#[derive(Debug, Clone)]
pub struct IssuedTabToken {
    /// The opaque token to send in `X-ZIM-Tab-Token`.
    pub token: String,
    /// The tab session the token is tied to.
    pub tab_id: Uuid,
    /// The instance the token is scoped to.
    pub instance_id: InstanceId,
    /// When the token expires.
    pub expires_at: DateTime<Utc>,
}

/// Failures of tab-token validation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TabTokenError {
    /// No such token (never issued, expired and pruned, or already
    /// revoked).
    #[error("tab token is unknown or revoked")]
    Unknown,
    /// The token existed but its TTL elapsed.
    #[error("tab token expired")]
    Expired,
}

struct TabTokenRecord {
    user_id: UserId,
    instance_id: InstanceId,
    tab_id: Uuid,
    grants: PermissionSet,
    expires_at: DateTime<Utc>,
}

/// The in-memory tab-token registry.
pub struct TabTokenManager {
    policy: TabTokenPolicy,
    records: RwLock<HashMap<String, TabTokenRecord>>,
}

impl TabTokenManager {
    /// Build a manager with a policy.
    pub fn new(policy: TabTokenPolicy) -> Self {
        Self {
            policy,
            records: RwLock::new(HashMap::new()),
        }
    }

    /// Issue a token for one tab's access to one instance.
    ///
    /// `tab_id` is the browser-integration layer's tab session
    /// identifier; a single tab may hold one token per instance,
    /// refreshed at will (each refresh issues a fresh secret and
    /// replaces the previous record for that `(tab, instance)`
    /// pair — old secrets stop validating immediately).
    pub fn issue(
        &self,
        user_id: UserId,
        instance_id: InstanceId,
        grants: PermissionSet,
        tab_id: Uuid,
        now: DateTime<Utc>,
    ) -> IssuedTabToken {
        let token = generate_token(32);
        // `from_std` only fails beyond ~262,000 years; a sane TTL always
        // converts (and a nonsensical one yields an immediately
        // expired token rather than a panic).
        let ttl = chrono::Duration::from_std(self.policy.ttl).unwrap_or_default();
        let expires_at = now + ttl;
        let record = TabTokenRecord {
            user_id,
            instance_id,
            tab_id,
            grants,
            expires_at,
        };
        {
            let mut records = self.records.write().expect("tab-token registry poisoned");
            // Replace any previous token for the same (tab,
            // instance) pair: one live secret per tab per instance.
            records.retain(|_, existing| {
                !(existing.tab_id == tab_id && existing.instance_id == instance_id)
            });
            records.insert(hash_token(&token), record);
        }
        IssuedTabToken {
            token,
            tab_id,
            instance_id,
            expires_at,
        }
    }

    /// Validate a presented token.
    ///
    /// Expired records are pruned as a side effect (a stale token
    /// and an unknown token are indistinguishable after the TTL,
    /// which is the property a revocation-by-time design wants).
    pub fn validate(&self, token: &str, now: DateTime<Utc>) -> Result<TabTokenView, TabTokenError> {
        let key = hash_token(token);
        let mut records = self.records.write().expect("tab-token registry poisoned");
        match records.get(&key) {
            Some(record) if record.expires_at <= now => {
                records.remove(&key);
                Err(TabTokenError::Expired)
            }
            Some(record) => Ok(TabTokenView {
                user_id: record.user_id,
                instance_id: record.instance_id,
                tab_id: record.tab_id,
                grants: record.grants.clone(),
                expires_at: record.expires_at,
            }),
            None => Err(TabTokenError::Unknown),
        }
    }

    /// Refresh a token's TTL (the tab-focus path).
    pub fn refresh(&self, token: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, TabTokenError> {
        let ttl = chrono::Duration::from_std(self.policy.ttl).unwrap_or_default();
        let key = hash_token(token);
        let mut records = self.records.write().expect("tab-token registry poisoned");
        match records.get_mut(&key) {
            Some(record) => {
                record.expires_at = now + ttl;
                Ok(record.expires_at)
            }
            None => Err(TabTokenError::Unknown),
        }
    }

    /// Revoke one token (tab navigation away, or a suspected
    /// theft).
    pub fn revoke(&self, token: &str) {
        let key = hash_token(token);
        self.records
            .write()
            .expect("tab-token registry poisoned")
            .remove(&key);
    }

    /// Revoke every token of one tab session (tab close).
    pub fn revoke_tab(&self, tab_id: Uuid) {
        self.records
            .write()
            .expect("tab-token registry poisoned")
            .retain(|_, record| record.tab_id != tab_id);
    }

    /// How many tokens are live (diagnostics and the §13
    /// per-tab subscriber limit's bookkeeping).
    pub fn live_token_count(&self) -> usize {
        self.records
            .read()
            .expect("tab-token registry poisoned")
            .len()
    }

    /// Drop expired records (amortized housekeeping for callers
    /// that validate rarely).
    pub fn prune(&self, now: DateTime<Utc>) {
        self.records
            .write()
            .expect("tab-token registry poisoned")
            .retain(|_, record| record.expires_at > now);
    }
}

/// Generate a tab session id (the browser-integration layer's tab
/// identifier, or a locally generated one for engine-internal
/// callers).
pub fn new_tab_id() -> Uuid {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // Version 4.
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // RFC 4122 variant.
    Uuid::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zim_engine_domain::{PermissionGrant, PermissionSet};

    fn grants() -> PermissionSet {
        let mut set = PermissionSet::new();
        set.grant(PermissionGrant::WebsocketConnect);
        set.grant(PermissionGrant::ControlConsole);
        set
    }

    fn fixed_now() -> DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn issue_validate_and_revoke_flow() {
        let manager = TabTokenManager::new(TabTokenPolicy::default());
        let user = UserId::new();
        let instance = InstanceId::new();
        let tab = new_tab_id();

        let issued = manager.issue(user, instance, grants(), tab, fixed_now());
        let view = manager.validate(&issued.token, fixed_now()).unwrap();
        assert_eq!(view.user_id, user);
        assert_eq!(view.instance_id, instance);
        assert_eq!(view.tab_id, tab);
        assert!(view.grants.contains(PermissionGrant::WebsocketConnect));

        manager.revoke(&issued.token);
        assert!(matches!(
            manager.validate(&issued.token, fixed_now()),
            Err(TabTokenError::Unknown)
        ));
    }

    #[test]
    fn expiry_and_refresh() {
        let policy = TabTokenPolicy {
            ttl: Duration::from_millis(20),
        };
        let manager = TabTokenManager::new(policy);
        let user = UserId::new();
        let instance = InstanceId::new();
        let tab = new_tab_id();

        let issued = manager.issue(user, instance, grants(), tab, fixed_now());
        std::thread::sleep(Duration::from_millis(30));
        assert!(matches!(
            manager.validate(&issued.token, fixed_now()),
            Err(TabTokenError::Expired)
        ));

        // A refreshed token lives past the original TTL.
        let issued = manager.issue(user, instance, grants(), tab, fixed_now());
        let refreshed_to = manager.refresh(&issued.token, fixed_now()).unwrap();
        assert!(refreshed_to > fixed_now());
    }

    #[test]
    fn revoke_tab_removes_all_of_the_tab() {
        let manager = TabTokenManager::new(TabTokenPolicy::default());
        let user = UserId::new();
        let tab = new_tab_id();

        let first = manager.issue(user, InstanceId::new(), grants(), tab, fixed_now());
        let second = manager.issue(user, InstanceId::new(), grants(), tab, fixed_now());
        assert_eq!(manager.live_token_count(), 2);

        manager.revoke_tab(tab);
        assert_eq!(manager.live_token_count(), 0);
        assert!(matches!(
            manager.validate(&first.token, fixed_now()),
            Err(TabTokenError::Unknown)
        ));
        assert!(matches!(
            manager.validate(&second.token, fixed_now()),
            Err(TabTokenError::Unknown)
        ));
    }

    #[test]
    fn reissue_replaces_the_previous_secret() {
        let manager = TabTokenManager::new(TabTokenPolicy::default());
        let user = UserId::new();
        let instance = InstanceId::new();
        let tab = new_tab_id();

        let old = manager.issue(user, instance, grants(), tab, fixed_now());
        let new = manager.issue(user, instance, grants(), tab, fixed_now());
        assert_ne!(old.token, new.token);
        // The old secret stopped validating immediately.
        assert!(matches!(
            manager.validate(&old.token, fixed_now()),
            Err(TabTokenError::Unknown)
        ));
        assert!(manager.validate(&new.token, fixed_now()).is_ok());
        assert_eq!(manager.live_token_count(), 1);
    }
}
