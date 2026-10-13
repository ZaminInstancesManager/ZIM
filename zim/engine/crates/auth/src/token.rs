//! Opaque token generation and at-rest token hashing.
//!
//! Session tokens and API key secrets are random byte strings from
//! the OS entropy source, encoded base64url (no padding — URL-safe
//! in cookies and headers). At rest they are stored as lowercase
//! SHA-256 hex digests: a database (or log) leak then does not yield
//! usable credentials, while lookup by hash stays O(1).

use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};

/// The base64url alphabet (RFC 4648 §5, without `=` padding).
const B64URL_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Generates `bytes` random bytes from the OS entropy source and
/// encodes them base64url without padding.
///
/// `bytes = 32` (256 bits) yields a 43-character token — the default
/// for sessions and API key secrets.
pub fn generate_token(bytes: usize) -> String {
    let mut raw = vec![0u8; bytes];
    OsRng.fill_bytes(&mut raw);
    to_base64url(&raw)
}

/// SHA-256 of the token as lowercase hex — the at-rest form.
pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    to_hex(&digest)
}

/// Length-constant base64url (no padding) encoder.
fn to_base64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64URL_ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(B64URL_ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        if chunk.len() > 1 {
            out.push(B64URL_ALPHABET[(triple >> 6) as usize & 0x3f] as char);
        }
        if chunk.len() > 2 {
            out.push(B64URL_ALPHABET[triple as usize & 0x3f] as char);
        }
    }
    out
}

/// Lowercase hex encoder (for digests).
fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_url_safe_and_correctly_sized() {
        for len in [16usize, 32, 33, 64] {
            let token = generate_token(len);
            assert_eq!(token.len(), len.div_ceil(3) * 4 - (3 - len % 3) % 3);
            assert!(token
                .chars()
                .all(|c| { c.is_ascii_alphanumeric() || c == '-' || c == '_' }));
        }
    }

    #[test]
    fn default_token_length_is_256_bits() {
        let token = generate_token(32);
        assert_eq!(token.len(), 43);
    }

    #[test]
    fn tokens_are_unique() {
        assert_ne!(generate_token(32), generate_token(32));
    }

    #[test]
    fn base64url_encoding_matches_reference_vectors() {
        // RFC 4648 §10 test vectors, with padding stripped and
        // `+`→`-`, `/`→`_` per §5.
        assert_eq!(to_base64url(b""), "");
        assert_eq!(to_base64url(b"f"), "Zg");
        assert_eq!(to_base64url(b"fo"), "Zm8");
        assert_eq!(to_base64url(b"foo"), "Zm9v");
        assert_eq!(to_base64url(b"foob"), "Zm9vYg");
        assert_eq!(to_base64url(b"fooba"), "Zm9vYmE");
        assert_eq!(to_base64url(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn hex_encoding_is_lowercase() {
        assert_eq!(to_hex(&[0x00, 0x0f, 0xff, 0xa5]), "000fffa5");
    }

    #[test]
    fn hash_token_is_sha256_hex() {
        // SHA-256("") = e3b0c442... (well-known empty-input digest).
        assert_eq!(
            hash_token(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(hash_token("zim").len(), 64);
        assert!(hash_token("zim")
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
