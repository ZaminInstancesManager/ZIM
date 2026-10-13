//! API key issuance, validation, and revocation.
//!
//! Per `docs/api/ZIM_API_SPEC.md` §3 and §10.2: API keys are bearer
//! tokens sent as `Authorization: Bearer {token}`, scoped to a
//! client-chosen subset of [`PermissionSet`], with optional expiry.
//!
//! Format: `zim_{key_id}_{secret}` where `key_id` is 16 lowercase
//! hex chars (also the lookup key) and `secret` is 32 random bytes
//! base64url-encoded (43 chars). Only the SHA-256 of the secret is
//! stored; the full key is returned once at issuance and never
//! retrievable afterwards. Revocation is by `key_id` (safe to log,
//! list, and show in UIs — it is not a secret).

use crate::token::{generate_token, hash_token};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::RwLock;
use thiserror::Error;
use zim_engine_domain::{PermissionSet, UserId};

/// The prefix every ZIM API key starts with.
pub const API_KEY_PREFIX: &str = "zim_";

/// An issued API key record. Stored keyed by `key_id`; the secret
/// exists only as `secret_hash`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyRecord {
    /// 16 lowercase hex chars; the lookup key. Not a secret.
    pub key_id: String,
    /// SHA-256 hex of the secret part. The secret itself is never
    /// stored.
    pub secret_hash: String,
    /// The owning user.
    pub user_id: UserId,
    /// The grants this key may exercise (subset of the owner's
    /// grants, enforced at issuance by the caller).
    pub scopes: PermissionSet,
    /// When the key was issued.
    pub created_at: DateTime<Utc>,
    /// Optional expiry; `None` = until revoked.
    pub expires_at: Option<DateTime<Utc>>,
    /// Last successful validation, if any.
    pub last_used_at: Option<DateTime<Utc>>,
}

/// A freshly issued API key: the full key (shown exactly once) plus
/// the record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedApiKey {
    /// The `zim_{key_id}_{secret}` bearer token. Copy it now; it
    /// cannot be retrieved later.
    pub full_key: String,
    /// The stored record.
    pub record: ApiKeyRecord,
}

/// Errors from API key validation and revocation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ApiKeyError {
    /// The presented token is not `zim_{hex16}_{base64url}`.
    #[error("malformed api key")]
    Malformed,
    /// No key with that `key_id` exists.
    #[error("unknown api key")]
    Unknown,
    /// The key was revoked.
    #[error("api key revoked")]
    Revoked,
    /// The key's expiry elapsed.
    #[error("api key expired")]
    Expired,
}

/// In-memory API key manager.
pub struct ApiKeyManager {
    keys: RwLock<HashMap<String, ApiKeyRecord>>,
}

impl ApiKeyManager {
    /// An empty manager.
    pub fn new() -> Self {
        Self {
            keys: RwLock::new(HashMap::new()),
        }
    }

    /// Issues a key for `user_id` with the given scopes and optional
    /// expiry.
    ///
    /// The caller (API layer) is responsible for verifying that
    /// `scopes` is a subset of what `user_id` may grant; the manager
    /// only stores it.
    pub fn issue(
        &self,
        user_id: &UserId,
        scopes: PermissionSet,
        expires_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> IssuedApiKey {
        // key_id: 8 random bytes as 16 lowercase hex chars.
        let key_id = generate_key_id();
        let secret = generate_token(32);
        let record = ApiKeyRecord {
            key_id: key_id.clone(),
            secret_hash: hash_token(&secret),
            user_id: *user_id,
            scopes,
            created_at: now,
            expires_at,
            last_used_at: None,
        };
        let full_key = format!("{API_KEY_PREFIX}{key_id}_{secret}");
        self.keys
            .write()
            .expect("api key map lock poisoned")
            .insert(key_id, record.clone());
        IssuedApiKey { full_key, record }
    }

    /// Validates a presented bearer token and records last-use.
    ///
    /// `Ok(record)` — the key is live; `Err(...)` distinguishes
    /// malformed / unknown / revoked / expired so the API layer can
    /// answer `401` uniformly but log the reason.
    pub fn validate(
        &self,
        full_key: &str,
        now: DateTime<Utc>,
    ) -> Result<ApiKeyRecord, ApiKeyError> {
        let (key_id, secret) = parse_full_key(full_key)?;
        let mut keys = self.keys.write().expect("api key map lock poisoned");
        let record = keys.get_mut(&key_id).ok_or(ApiKeyError::Unknown)?;
        if record.secret_hash != hash_token(&secret) {
            // Right key_id, wrong secret: treat as unknown to avoid
            // confirming key_id existence to a guesser.
            return Err(ApiKeyError::Unknown);
        }
        if let Some(expiry) = record.expires_at {
            if now > expiry {
                return Err(ApiKeyError::Expired);
            }
        }
        record.last_used_at = Some(now);
        Ok(record.clone())
    }

    /// Revokes by `key_id`.
    pub fn revoke(&self, key_id: &str) -> Result<(), ApiKeyError> {
        self.keys
            .write()
            .expect("api key map lock poisoned")
            .remove(key_id)
            .map(|_| ())
            .ok_or(ApiKeyError::Unknown)
    }

    /// The records belonging to `user_id` (for the list endpoint).
    /// Records carry hashes, not secrets — safe to serialize.
    pub fn list_for_user(&self, user_id: &UserId) -> Vec<ApiKeyRecord> {
        self.keys
            .read()
            .expect("api key map lock poisoned")
            .values()
            .filter(|record| record.user_id == *user_id)
            .cloned()
            .collect()
    }

    /// Number of live keys.
    pub fn len(&self) -> usize {
        self.keys.read().expect("api key map lock poisoned").len()
    }

    /// Whether the manager holds no keys.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for ApiKeyManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Generates a 16-hex-char key id from 8 random bytes.
fn generate_key_id() -> String {
    use rand_core::{OsRng, RngCore};
    let mut bytes = [0u8; 8];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Splits `zim_{key_id}_{secret}` into its parts.
fn parse_full_key(full_key: &str) -> Result<(String, String), ApiKeyError> {
    let rest = full_key
        .strip_prefix(API_KEY_PREFIX)
        .ok_or(ApiKeyError::Malformed)?;
    let (key_id, secret) = rest.split_once('_').ok_or(ApiKeyError::Malformed)?;
    if key_id.len() != 16
        || !key_id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(ApiKeyError::Malformed);
    }
    if secret.is_empty()
        || !secret
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ApiKeyError::Malformed);
    }
    Ok((key_id.to_owned(), secret.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zim_engine_domain::PermissionGrant;

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn scoped_manager() -> (ApiKeyManager, IssuedApiKey) {
        let manager = ApiKeyManager::new();
        let mut scopes = PermissionSet::new();
        scopes
            .grant(PermissionGrant::ControlStart)
            .grant(PermissionGrant::FileRead);
        let issued = manager.issue(&UserId::new(), scopes, None, fixed_now());
        (manager, issued)
    }

    #[test]
    fn full_key_has_the_documented_shape() {
        let (_, issued) = scoped_manager();
        // The secret is base64url, which itself may contain '_' — so
        // parse structurally (prefix, 16 hex chars, separator, 43
        // base64url chars), never by splitting on every '_'.
        let rest = issued.full_key.strip_prefix("zim_").unwrap();
        let (key_id, secret) = rest.split_once('_').unwrap();
        assert_eq!(key_id.len(), 16);
        assert!(key_id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(secret.len(), 43);
        assert!(secret
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn validate_round_trips_and_tracks_use() {
        let (manager, issued) = scoped_manager();
        let now = fixed_now() + chrono::Duration::minutes(5);
        let record = manager.validate(&issued.full_key, now).unwrap();
        assert!(record.scopes.contains(PermissionGrant::ControlStart));
        assert!(!record.scopes.contains(PermissionGrant::ControlStop));
        assert_eq!(record.last_used_at, Some(now));
    }

    #[test]
    fn malformed_keys_are_rejected() {
        let (manager, _) = scoped_manager();
        let now = fixed_now();
        for bad in [
            "",
            "zim_",
            "zim_onlyprefix",
            "prefix_0123456789abcdef_secret",
            "zim_SHORT_secret-value-here",
            "zim_UPPERCASE01234567_secret",
            "zim_0123456789abcdef_",
        ] {
            assert!(
                matches!(
                    manager.validate(bad, now),
                    Err(ApiKeyError::Malformed | ApiKeyError::Unknown)
                ),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn wrong_secret_is_unknown_not_revoked() {
        let (manager, issued) = scoped_manager();
        let forged = format!(
            "zim_{}_{}",
            issued.record.key_id, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        );
        assert_eq!(
            manager.validate(&forged, fixed_now()).unwrap_err(),
            ApiKeyError::Unknown
        );
    }

    #[test]
    fn expiry_is_enforced() {
        let manager = ApiKeyManager::new();
        let issued = manager.issue(
            &UserId::new(),
            PermissionSet::new(),
            Some(fixed_now() + chrono::Duration::hours(1)),
            fixed_now(),
        );
        assert!(manager
            .validate(
                &issued.full_key,
                fixed_now() + chrono::Duration::minutes(59)
            )
            .is_ok());
        assert_eq!(
            manager
                .validate(
                    &issued.full_key,
                    fixed_now() + chrono::Duration::minutes(61)
                )
                .unwrap_err(),
            ApiKeyError::Expired
        );
    }

    #[test]
    fn revoke_kills_the_key() {
        let (manager, issued) = scoped_manager();
        manager.revoke(&issued.record.key_id).unwrap();
        assert_eq!(
            manager.validate(&issued.full_key, fixed_now()).unwrap_err(),
            ApiKeyError::Unknown
        );
        assert_eq!(
            manager.revoke(&issued.record.key_id),
            Err(ApiKeyError::Unknown)
        );
    }

    #[test]
    fn list_for_user_is_scoped() {
        let manager = ApiKeyManager::new();
        let alice = UserId::new();
        let bob = UserId::new();
        manager.issue(&alice, PermissionSet::new(), None, fixed_now());
        manager.issue(&alice, PermissionSet::new(), None, fixed_now());
        manager.issue(&bob, PermissionSet::new(), None, fixed_now());
        assert_eq!(manager.list_for_user(&alice).len(), 2);
        assert_eq!(manager.list_for_user(&bob).len(), 1);
        assert_eq!(manager.len(), 3);
    }

    #[test]
    fn records_never_carry_the_secret() {
        let (manager, issued) = scoped_manager();
        let listed = manager.list_for_user(&issued.record.user_id);
        assert_eq!(listed.len(), 1);
        // The full key must not appear anywhere in the record form.
        let debug = format!("{:?}", listed[0]);
        assert!(!debug.contains(&issued.full_key));
        assert_ne!(listed[0].secret_hash, issued.full_key);
    }
}
