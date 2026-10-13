//! Argon2id password hashing and the password policy.
//!
//! Hashes are PHC strings (`$argon2id$v=19$m=19456,t=2,p=1$...`),
//! which embed the parameters, so verification does not need the
//! policy that produced the hash. The hashing parameters are fixed
//! at the OWASP-recommended Argon2id profile (m=19456 KiB, t=2,
//! p=1) rather than configurable per deployment: configurability
//! here is a foot-gun (weak params are invisible in review), and the
//! profile can be raised engine-wide with a coordinated rehash.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use rand_core::OsRng;
use thiserror::Error;

/// Policy for acceptable plaintext passwords.
///
/// Only length bounds are enforced at the domain layer. Composition
/// rules (character classes, common-password blocklists) are the
/// frontend's and API layer's concern; the engine rejects only what
/// cannot be safely hashed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasswordPolicy {
    /// Minimum plaintext length (default 8, matching Pterodactyl's
    /// panel default).
    pub min_length: usize,
    /// Maximum plaintext length (default 512). Argon2's runtime is
    /// linear in input length, so a bound is a DoS guard.
    pub max_length: usize,
}

impl Default for PasswordPolicy {
    fn default() -> Self {
        Self {
            min_length: 8,
            max_length: 512,
        }
    }
}

/// Errors from password validation and hashing.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PasswordError {
    /// The password is empty.
    #[error("password must not be empty")]
    Empty,
    /// The password is shorter than the policy minimum.
    #[error("password must be at least {min} characters")]
    TooShort {
        /// The policy minimum.
        min: usize,
    },
    /// The password exceeds the policy maximum (DoS guard).
    #[error("password must be at most {max} characters")]
    TooLong {
        /// The policy maximum.
        max: usize,
    },
    /// The stored hash is not a valid PHC string (corrupt row or
    /// foreign format).
    #[error("stored password hash is malformed")]
    MalformedHash,
    /// Hashing failed for an environmental reason (entropy source,
    /// param limits).
    #[error("password hashing failed: {0}")]
    Hashing(String),
}

/// Validates a plaintext password against the policy.
pub fn validate_password(policy: &PasswordPolicy, plain: &str) -> Result<(), PasswordError> {
    if plain.is_empty() {
        return Err(PasswordError::Empty);
    }
    if plain.len() < policy.min_length {
        return Err(PasswordError::TooShort {
            min: policy.min_length,
        });
    }
    if plain.len() > policy.max_length {
        return Err(PasswordError::TooLong {
            max: policy.max_length,
        });
    }
    Ok(())
}

/// Hashes a password with Argon2id (random 16-byte salt) after
/// validating it against `policy`.
///
/// Returns a PHC string suitable for `User::password_hash`. The
/// parameters are the OWASP-recommended Argon2id profile
/// (m=19456 KiB, t=2, p=1); see the module docs for why they are
/// fixed rather than configurable.
pub fn hash_password(policy: &PasswordPolicy, plain: &str) -> Result<String, PasswordError> {
    validate_password(policy, plain)?;
    let params = Params::new(19456, 2, 1, None)
        .map_err(|e| PasswordError::Hashing(format!("invalid argon2 params: {e}")))?;
    hash_with_params(plain, &params)
}

/// Hashes with explicit parameters (used by tests to lower the cost;
/// production code always goes through [`hash_password`]).
#[cfg(test)]
pub(crate) fn hash_password_weak_for_tests(plain: &str) -> Result<String, PasswordError> {
    hash_with_params(
        plain,
        &Params::new(64, 1, 1, None).map_err(|e| PasswordError::Hashing(e.to_string()))?,
    )
}

fn hash_with_params(plain: &str, params: &Params) -> Result<String, PasswordError> {
    let salt = SaltString::generate(&mut OsRng);
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params.clone());
    let hash = argon
        .hash_password(plain.as_bytes(), &salt)
        .map_err(|e| PasswordError::Hashing(e.to_string()))?;
    Ok(hash.to_string())
}

/// Verifies a plaintext password against a stored PHC string.
///
/// `Ok(true)` = match, `Ok(false)` = wrong password, `Err(MalformedHash)`
/// = the stored string is not a valid PHC hash. Malformed hashes must
/// be treated as a failed login (never as a match), and surfaced so
/// the caller can log the corrupt row.
pub fn verify_password(plain: &str, phc: &str) -> Result<bool, PasswordError> {
    let parsed = PasswordHash::new(phc).map_err(|_| PasswordError::MalformedHash)?;
    // Verification uses the parameters embedded in the PHC string.
    match Argon2::default().verify_password(plain.as_bytes(), &parsed) {
        Ok(()) => Ok(true),
        Err(argon2::password_hash::Error::Password) => Ok(false),
        Err(_) => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests use the weak-parameter hasher to keep the suite fast;
    // the parameter handling itself is covered by parsing the PHC
    // string back.

    #[test]
    fn policy_defaults_match_pterodactyl() {
        let policy = PasswordPolicy::default();
        assert_eq!(policy.min_length, 8);
        assert_eq!(policy.max_length, 512);
    }

    #[test]
    fn short_and_long_passwords_are_rejected() {
        let policy = PasswordPolicy::default();
        assert_eq!(validate_password(&policy, ""), Err(PasswordError::Empty));
        assert_eq!(
            validate_password(&policy, "1234567"),
            Err(PasswordError::TooShort { min: 8 })
        );
        assert!(validate_password(&policy, "12345678").is_ok());
        let too_long = "x".repeat(513);
        assert_eq!(
            validate_password(&policy, &too_long),
            Err(PasswordError::TooLong { max: 512 })
        );
    }

    #[test]
    fn hash_is_a_phc_argon2id_string() {
        let hash = hash_password_weak_for_tests("hunter2hunter2").unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(hash.contains("v=19"));
    }

    #[test]
    fn verify_round_trips() {
        let hash = hash_password_weak_for_tests("correct horse battery staple").unwrap();
        assert!(verify_password("correct horse battery staple", &hash).unwrap());
        assert!(!verify_password("Tr0ub4dor&3", &hash).unwrap());
    }

    #[test]
    fn verify_rejects_malformed_hashes() {
        assert_eq!(
            verify_password("whatever", "not-a-hash"),
            Err(PasswordError::MalformedHash)
        );
        assert_eq!(
            verify_password("whatever", ""),
            Err(PasswordError::MalformedHash)
        );
    }

    #[test]
    fn hashes_are_salted_and_unique() {
        // Same plaintext, two hashes, both verify, and the strings
        // differ (random salt).
        let a = hash_password_weak_for_tests("same password").unwrap();
        let b = hash_password_weak_for_tests("same password").unwrap();
        assert_ne!(a, b);
        assert!(verify_password("same password", &a).unwrap());
        assert!(verify_password("same password", &b).unwrap());
    }

    #[test]
    fn production_hasher_uses_owasp_params() {
        // The real (non-test) path embeds m=19456,t=2,p=1.
        let hash = hash_password(&PasswordPolicy::default(), "production params!").unwrap();
        assert!(hash.contains("m=19456"));
        assert!(hash.contains("t=2"));
        assert!(hash.contains("p=1"));
    }
}
