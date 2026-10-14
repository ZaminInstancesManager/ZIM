//! Frontend-package signature verification (§3.4).
//!
//! A package is signed by embedding an ed25519 signature over the
//! canonical manifest JSON (`signature` removed). The engine
//! consults a trust store — the built-in ZaminMC development key
//! plus any operator-added keys in `{data_dir}/frontend/
//! trusted-keys.json` — and classifies the result:
//!
//! - [`Trusted`]: signed by a known key and the signature verifies.
//! - [`Invalid`]: signed by a known key and the signature does NOT
//!   verify — a hard reject that no approval can override.
//! - [`Untrusted`]: signed by an unknown key (or unsigned) —
//!   installable only with explicit admin approval.
//!
//! The bundled default frontend is trusted by construction (it is
//! compiled into the engine binary, not uploaded), which is why it
//! carries no signature.
//!
//! Development key policy: `keys/zaminmc-dev.key` is the repo-held
//! development signing key (see that directory's README); production
//! signing keys rotate out-of-band before any signed release
//! channel exists.

use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use crate::manifest::FrontendManifest;

/// The built-in ZaminMC development verifying key (base64, raw 32
/// bytes), embedded from `keys/zaminmc-dev.pub`.
pub const BUILTIN_TRUSTED_KEY: &str = include_str!("../keys/zaminmc-dev.pub");

/// The development *signing* key (base64, raw 32 bytes). Committed
/// openly — it signs test fixtures and development builds only; see
/// the module docs for the rotation policy. Compiled into tests and
/// the `sign` helper path, never into any release trust decision.
pub const DEV_SIGNING_KEY: &str = include_str!("../keys/zaminmc-dev.key");

/// The outcome of classifying a manifest's signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureCheck {
    /// Signed by a trusted key, and it verifies.
    Trusted,
    /// Signed by a trusted key, and it does NOT verify.
    Invalid,
    /// Signed by an unknown key — activatable only with explicit
    /// admin approval.
    UntrustedAuthor,
    /// Unsigned — activatable only with explicit admin approval
    /// (the bundled default is the one unsigned exception, and it
    /// never passes through this path).
    Unsigned,
}

impl SignatureCheck {
    /// The wire label for API surfaces.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::Invalid => "invalid",
            Self::UntrustedAuthor => "untrusted",
            Self::Unsigned => "unsigned",
        }
    }

    /// Whether activation requires explicit admin approval.
    pub fn requires_approval(&self) -> bool {
        matches!(self, Self::UntrustedAuthor | Self::Unsigned)
    }

    /// Whether activation must be refused outright.
    pub fn is_hard_reject(&self) -> bool {
        matches!(self, Self::Invalid)
    }
}

/// A resolved trust store: the built-in keys plus any the operator
/// added in the data directory.
#[derive(Debug, Clone)]
pub struct TrustStore {
    keys: Vec<[u8; 32]>,
}

impl TrustStore {
    /// The built-in trust store (ZaminMC's key).
    pub fn builtin() -> Self {
        Self::with_extra_keys(std::iter::empty())
    }

    /// Built-in keys plus base64-encoded operator keys (unknown
    /// entries are skipped — a malformed operator file must not
    /// brick the engine's frontend serving).
    pub fn with_extra_keys(extra: impl Iterator<Item = String>) -> Self {
        let mut keys = vec![decode_key(BUILTIN_TRUSTED_KEY).expect("built-in key is valid")];
        for entry in extra {
            if let Some(key) = decode_key(&entry) {
                keys.push(key);
            }
        }
        Self { keys }
    }

    /// Classify a manifest's signature against this store.
    pub fn check(&self, manifest: &FrontendManifest) -> SignatureCheck {
        let Some(signature) = &manifest.signature else {
            return SignatureCheck::Unsigned;
        };
        let Some(key_bytes) = decode_key(&signature.public_key) else {
            // Shape was validated at parse; a decode failure here
            // means whitespace/trailing junk: treat as unknown.
            return SignatureCheck::UntrustedAuthor;
        };
        let Some(value) = decode_signature(&signature.value) else {
            return SignatureCheck::Invalid;
        };
        if !self.keys.contains(&key_bytes) {
            return SignatureCheck::UntrustedAuthor;
        }
        let verifying =
            VerifyingKey::from_bytes(&key_bytes).expect("a 32-byte key always constructs");
        let message = manifest.canonical_json();
        match verifying.verify(message.as_bytes(), &value) {
            Ok(()) => SignatureCheck::Trusted,
            Err(_) => SignatureCheck::Invalid,
        }
    }
}

/// Decode a base64 raw-32 key.
fn decode_key(value: &str) -> Option<[u8; 32]> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value.trim())
        .ok()?;
    let mut key = [0u8; 32];
    if bytes.len() != 32 {
        return None;
    }
    key.copy_from_slice(&bytes);
    Some(key)
}

/// Decode a base64 raw-64 signature.
fn decode_signature(value: &str) -> Option<Signature> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value.trim())
        .ok()?;
    let mut raw = [0u8; 64];
    if bytes.len() != 64 {
        return None;
    }
    raw.copy_from_slice(&bytes);
    Some(Signature::from_bytes(&raw))
}

/// Sign a manifest with the development signing key (test and
/// tooling path). Returns the base64 signature to embed as
/// `signature.value`.
pub fn dev_sign(manifest: &FrontendManifest) -> String {
    sign_with(manifest, DEV_SIGNING_KEY)
}

/// Sign a manifest with an explicit base64 seed.
pub fn sign_with(manifest: &FrontendManifest, seed_b64: &str) -> String {
    let seed = decode_key(seed_b64).expect("valid 32-byte seed");
    let signing = SigningKey::from_bytes(&seed);
    let signature = signing.sign(manifest.canonical_json().as_bytes());
    base64::engine::general_purpose::STANDARD.encode(signature.to_bytes())
}

/// The verifying-key half of an explicit base64 seed (for building
/// fixture trust files).
pub fn public_key_of(seed_b64: &str) -> String {
    let seed = decode_key(seed_b64).expect("valid 32-byte seed");
    let signing = SigningKey::from_bytes(&seed);
    base64::engine::general_purpose::STANDARD.encode(signing.verifying_key().as_bytes())
}

/// Sign with the development key AND stamp the matching signature
/// block onto the manifest (the fixture-authoring helper).
pub fn dev_sign_manifest(manifest: &mut FrontendManifest) {
    let value = dev_sign(manifest);
    manifest.signature = Some(crate::manifest::SignatureInfo {
        algorithm: "ed25519".to_owned(),
        public_key: BUILTIN_TRUSTED_KEY.trim().to_owned(),
        value,
    });
}

/// Exposed for tests that exercise raw verification paths.
#[allow(dead_code)]
pub(crate) fn verify_raw(public_key_b64: &str, message: &[u8], signature_b64: &str) -> bool {
    let Some(key) = decode_key(public_key_b64) else {
        return false;
    };
    let Some(signature) = decode_signature(signature_b64) else {
        return false;
    };
    VerifyingKey::from_bytes(&key)
        .expect("a 32-byte key always constructs")
        .verify(message, &signature)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_key_decodes() {
        assert!(decode_key(BUILTIN_TRUSTED_KEY).is_some());
        assert!(decode_key(DEV_SIGNING_KEY).is_some());
    }

    #[test]
    fn dev_keypair_is_a_real_pair() {
        // The committed pair must actually match: a signature from
        // the dev seed verifies under the embedded public key.
        assert!(verify_raw(
            BUILTIN_TRUSTED_KEY,
            b"zim development keypair self-check",
            &{
                let signing =
                    SigningKey::from_bytes(&decode_key(DEV_SIGNING_KEY).expect("valid seed"));
                let signature = signing.sign(b"zim development keypair self-check");
                base64::engine::general_purpose::STANDARD.encode(signature.to_bytes())
            },
        ));
    }

    #[test]
    fn classification_follows_the_trust_store() {
        // Build a manifest and sign it with a *different* random key.
        let mut manifest = crate::manifest::FrontendManifest {
            id: "org.example.frontend.signed".to_owned(),
            manifest_version: 1,
            version: "0.1.0".to_owned(),
            name: "Signed".to_owned(),
            description: String::new(),
            author: "Example".to_owned(),
            license: "MIT".to_owned(),
            api_version: "v1".to_owned(),
            engines: std::collections::BTreeMap::from([("zim".to_owned(), ">=0.0.1".to_owned())]),
            full_replacement: false,
            smoke_test: None,
            page_surfaces: Vec::new(),
            entrypoints: std::collections::BTreeMap::from([(
                "newtab".to_owned(),
                "pages/newtab.html".to_owned(),
            )]),
            assets: Vec::new(),
            permissions: Vec::new(),
            accessibility: None,
            signature: None,
        };

        // Unsigned.
        assert_eq!(
            TrustStore::builtin().check(&manifest),
            SignatureCheck::Unsigned
        );

        // Signed with an unknown key → untrusted.
        let unknown_seed = [7u8; 32];
        let unknown_b64 = base64::engine::general_purpose::STANDARD.encode(unknown_seed);
        let value = sign_with(&manifest, &unknown_b64);
        manifest.signature = Some(crate::manifest::SignatureInfo {
            algorithm: "ed25519".to_owned(),
            public_key: public_key_of(&unknown_b64),
            value,
        });
        assert_eq!(
            TrustStore::builtin().check(&manifest),
            SignatureCheck::UntrustedAuthor
        );

        // Same key added to the store → trusted.
        let store = TrustStore::with_extra_keys([public_key_of(&unknown_b64)].into_iter());
        assert_eq!(store.check(&manifest), SignatureCheck::Trusted);

        // Tampered canonical form → invalid under a known key.
        manifest.name = "Tampered".to_owned();
        assert_eq!(store.check(&manifest), SignatureCheck::Invalid);
    }
}
