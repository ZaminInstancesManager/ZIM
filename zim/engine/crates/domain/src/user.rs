//! User and instance-scoped Permission entities.
//!
//! # Source of truth
//!
//! `User` and `Permission` (the instance-scoped grant row) mirror
//! `docs/architecture/RUST_ENGINE.md` §3. The permission vocabulary
//! itself (`PermissionGrant` / `PermissionSet`) lives in the
//! `permission` module; this module holds the *entities* that
//! reference it.
//!
//! Local accounts carry an Argon2id `password_hash`; OAuth accounts
//! carry an `oauth_subject`. The hashing itself lives in
//! `zim_engine_auth` (M4) — the domain layer only carries the opaque
//! hash string.

use crate::id::{InstanceId, UserId};
use crate::permission::PermissionSet;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A ZIM user account (local or OAuth-backed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    /// Strongly-typed user identifier.
    pub id: UserId,
    /// Login email (lowercase; unique).
    pub email: String,
    /// Display username (unique).
    pub username: String,
    /// Legacy Pterodactyl external identifier (migration compat).
    pub external_id: Option<String>,
    /// Argon2id password hash for local accounts (None for
    /// OAuth-only accounts).
    pub password_hash: Option<String>,
    /// OAuth subject identifier (`provider:subject`) for OAuth
    /// accounts (None for local accounts).
    pub oauth_subject: Option<String>,
    /// Whether the user can administer instances they do not own.
    pub is_admin: bool,
    /// Whether the user is a root administrator (cannot be deleted,
    /// bypasses permission checks).
    pub root_admin: bool,
}

impl User {
    /// Validate the invariants the constructor cannot enforce.
    pub fn validate(&self) -> Result<(), UserError> {
        // Light structural validation: full RFC 5322 validation is
        // the API layer's job (M5); the domain layer enforces the
        // shape that the persistence layer relies on.
        let email = &self.email;
        let at_count = email.matches('@').count();
        if at_count != 1
            || email.starts_with('@')
            || email.ends_with('@')
            || email
                .split('@')
                .nth(1)
                .is_some_and(|domain| domain.is_empty())
        {
            return Err(UserError::MalformedEmail);
        }
        if email != &email.to_lowercase() {
            return Err(UserError::EmailNotLowercase);
        }
        let username = &self.username;
        if username.is_empty() || username.len() > 64 {
            return Err(UserError::MalformedUsername);
        }
        if !username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        {
            return Err(UserError::MalformedUsername);
        }
        if self.password_hash.is_none() && self.oauth_subject.is_none() {
            return Err(UserError::NoCredential);
        }
        Ok(())
    }
}

/// Domain-level validation errors for User.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UserError {
    /// The email is not a plausible address.
    #[error("malformed email address")]
    MalformedEmail,
    /// The email is not stored lowercase (canonical form).
    #[error("email must be stored lowercase")]
    EmailNotLowercase,
    /// The username is empty, over 64 chars, or uses characters
    /// outside `[A-Za-z0-9_.-]`.
    #[error("malformed username")]
    MalformedUsername,
    /// Neither a local password hash nor an OAuth subject is set —
    /// the account has no way to authenticate.
    #[error("user must have a password hash or an oauth subject")]
    NoCredential,
}

/// An instance-scoped permission grant row.
///
/// Binds a `PermissionSet` to a (user, instance) pair. Root admins
/// bypass these rows entirely; `is_admin` users still need rows for
/// instances they do not own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Permission {
    /// The user the grants apply to.
    pub user_id: UserId,
    /// The instance the grants are scoped to.
    pub instance_id: InstanceId,
    /// The granted permission set.
    pub grants: PermissionSet,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::PermissionGrant;

    fn sample_user() -> User {
        User {
            id: UserId::new(),
            email: "ops@example.org".to_owned(),
            username: "ops.zamin".to_owned(),
            external_id: Some("p-legacy-7".to_owned()),
            password_hash: Some("$argon2id$v=19$m=65536,t=3,p=1$0 DAY".to_owned()),
            oauth_subject: None,
            is_admin: false,
            root_admin: false,
        }
    }

    #[test]
    fn user_round_trips_through_serde() {
        let user = sample_user();
        let json = serde_json::to_string(&user).unwrap();
        let back: User = serde_json::from_str(&json).unwrap();
        assert_eq!(user, back);
    }

    #[test]
    fn user_validate_accepts_sample() {
        assert!(sample_user().validate().is_ok());
    }

    #[test]
    fn oauth_only_user_is_valid() {
        let mut user = sample_user();
        user.password_hash = None;
        user.oauth_subject = Some("discord:1234567890".to_owned());
        assert!(user.validate().is_ok());
    }

    #[test]
    fn user_without_any_credential_is_rejected() {
        let mut user = sample_user();
        user.password_hash = None;
        user.oauth_subject = None;
        assert!(matches!(user.validate(), Err(UserError::NoCredential)));
    }

    #[test]
    fn malformed_emails_are_rejected() {
        for bad in ["", "no-at-sign", "@no-local", "no-domain@", "a@b@c"] {
            let mut user = sample_user();
            user.email = bad.to_owned();
            assert!(user.validate().is_err(), "email {bad:?} should be rejected");
        }
    }

    #[test]
    fn uppercase_email_is_rejected() {
        let mut user = sample_user();
        user.email = "Ops@Example.ORG".to_owned();
        assert!(matches!(user.validate(), Err(UserError::EmailNotLowercase)));
    }

    #[test]
    fn malformed_usernames_are_rejected() {
        for bad in ["", " ", "has space", "tab\tchar", "uni-nąme"] {
            let mut user = sample_user();
            user.username = bad.to_owned();
            assert!(
                user.validate().is_err(),
                "username {bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn permission_round_trips_through_serde() {
        let permission = Permission {
            user_id: UserId::new(),
            instance_id: InstanceId::new(),
            grants: {
                let mut set = PermissionSet::new();
                set.grant(PermissionGrant::ControlStart)
                    .grant(PermissionGrant::ControlStop)
                    .grant(PermissionGrant::FileRead);
                set
            },
        };
        let json = serde_json::to_string(&permission).unwrap();
        let back: Permission = serde_json::from_str(&json).unwrap();
        assert_eq!(permission, back);
        assert!(back.grants.contains(PermissionGrant::ControlStart));
    }
}
