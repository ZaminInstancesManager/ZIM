//! Addon signature classification (§4.1's `signature` block, §4.11's
//! install gate).
//!
//! The signature block shape and the trust semantics are shared with
//! frontend packages (§3.4): an ed25519 signature over the canonical
//! manifest JSON, classified against the engine's trust store —
//!
//! - [`Trusted`]: signed by a known key and it verifies —
//!   auto-installable.
//! - [`Invalid`]: signed by a known key and it does NOT verify — a
//!   hard reject no admin approval can override.
//! - [`UntrustedAuthor`] / [`Unsigned`]: installable only with
//!   admin approval — which, under §4.11's upload flow, is exactly
//!   what an admin's `POST /api/v1/extensions` is.
//!
//! The trust store is the frontend loader's (`zim_engine_frontend_loader`):
//! the built-in ZaminMC development key plus any operator-added keys,
//! with addon-specific extra keys loaded from
//! `{data_dir}/extensions/trusted-keys.json` (same file format, a
//! separate file so the two surfaces stay independently operable).

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use zim_engine_frontend_loader::signature::{
    SignatureCheck, TrustStore, BUILTIN_TRUSTED_KEY, DEV_SIGNING_KEY,
};

use crate::manifest::AddonManifest;

/// Classify an addon manifest's signature against the trust store.
pub fn classify(manifest: &AddonManifest, store: &TrustStore) -> SignatureCheck {
    match &manifest.signature {
        Some(block) => store.check_block(&manifest.canonical_json(), block),
        None => SignatureCheck::Unsigned,
    }
}

/// Load the addon trust store: the built-in keys plus the
/// operator's `{data_dir}/extensions/trusted-keys.json` entries
/// (a malformed or missing file yields just the built-ins — an
/// operator file problem must not brick addon serving).
pub fn trust_store(data_dir: &std::path::Path) -> TrustStore {
    let path = data_dir.join("extensions").join("trusted-keys.json");
    let extra = std::fs::read_to_string(&path).unwrap_or_default();
    let keys = serde_json::from_str::<TrustedKeyFile>(&extra)
        .map(|file| {
            file.keys
                .into_iter()
                .map(|entry| entry.public_key)
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();
    TrustStore::with_extra_keys(keys.into_iter())
}

#[derive(Debug, serde::Deserialize)]
struct TrustedKeyFile {
    #[serde(default)]
    keys: Vec<TrustedKeyEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct TrustedKeyEntry {
    #[serde(default)]
    #[allow(dead_code)] // id is informational; the key is what matters
    id: String,
    public_key: String,
}

/// Sign an addon manifest with the development signing key (test and
/// tooling path). Returns the base64 signature for
/// `manifest.signature.value`.
pub fn dev_sign(manifest: &AddonManifest) -> String {
    let seed = decode_key(DEV_SIGNING_KEY).expect("the development seed is a valid 32-byte key");
    let signing = SigningKey::from_bytes(&seed);
    let signature = signing.sign(manifest.canonical_json().as_bytes());
    base64::engine::general_purpose::STANDARD.encode(signature.to_bytes())
}

/// Stamp the development key's signature block onto a manifest (the
/// fixture-authoring helper).
pub fn dev_sign_manifest(manifest: &mut AddonManifest) {
    let value = dev_sign(manifest);
    manifest.signature = Some(zim_engine_frontend_loader::manifest::SignatureInfo {
        algorithm: "ed25519".to_owned(),
        public_key: BUILTIN_TRUSTED_KEY.trim().to_owned(),
        value,
    });
}

/// The verifying-key half of an explicit base64 seed (for building
/// fixture trust files).
pub fn public_key_of(seed_b64: &str) -> String {
    let seed = decode_key(seed_b64).expect("valid 32-byte seed");
    let signing = SigningKey::from_bytes(&seed);
    base64::engine::general_purpose::STANDARD.encode(signing.verifying_key().to_bytes())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::InstanceScope;
    use std::collections::BTreeMap;

    fn unsigned_manifest() -> AddonManifest {
        AddonManifest {
            id: "org.example.signed-addon".to_owned(),
            manifest_version: 1,
            version: "0.1.0".to_owned(),
            name: "Signed Addon".to_owned(),
            description: String::new(),
            author: "Example".to_owned(),
            license: "MIT".to_owned(),
            api_version: "v1".to_owned(),
            engines: BTreeMap::from([("zim".to_owned(), ">=0.0.1".to_owned())]),
            permissions: vec![],
            instance_scope: InstanceScope::None,
            page_surfaces: vec![],
            entrypoints: BTreeMap::new(),
            sidebar_items: vec![],
            context_menu_items: vec![],
            commands: vec![],
            event_subscriptions: vec![],
            dependencies: vec![],
            signature: None,
        }
    }

    #[test]
    fn unsigned_classifies_unsigned() {
        let manifest = unsigned_manifest();
        assert_eq!(classify(&manifest, &TrustStore::builtin()), SignatureCheck::Unsigned);
    }

    #[test]
    fn dev_signed_classifies_trusted() {
        let mut manifest = unsigned_manifest();
        dev_sign_manifest(&mut manifest);
        assert_eq!(classify(&manifest, &TrustStore::builtin()), SignatureCheck::Trusted);
    }

    #[test]
    fn tampered_dev_signature_classifies_invalid() {
        let mut manifest = unsigned_manifest();
        dev_sign_manifest(&mut manifest);
        // Tamper: change the name after signing.
        manifest.name = "Tampered".to_owned();
        assert_eq!(classify(&manifest, &TrustStore::builtin()), SignatureCheck::Invalid);
    }

    #[test]
    fn unknown_author_classifies_untrusted() {
        let mut manifest = unsigned_manifest();
        let foreign_seed = [7u8; 32];
        let signing = SigningKey::from_bytes(&foreign_seed);
        let value = base64::engine::general_purpose::STANDARD
            .encode(signing.sign(manifest.canonical_json().as_bytes()).to_bytes());
        manifest.signature = Some(zim_engine_frontend_loader::manifest::SignatureInfo {
            algorithm: "ed25519".to_owned(),
            public_key: public_key_of(&base64::engine::general_purpose::STANDARD.encode(foreign_seed)),
            value,
        });
        assert_eq!(
            classify(&manifest, &TrustStore::builtin()),
            SignatureCheck::UntrustedAuthor
        );
        // …but the operator adding that key promotes it to trusted.
        let store = TrustStore::with_extra_keys(
            [public_key_of(&base64::engine::general_purpose::STANDARD.encode(foreign_seed))]
                .into_iter(),
        );
        assert_eq!(classify(&manifest, &store), SignatureCheck::Trusted);
    }
}
