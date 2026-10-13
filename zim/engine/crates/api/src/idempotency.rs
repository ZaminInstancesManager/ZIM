//! `Idempotency-Key` replay store (§12).
//!
//! Mutating requests may carry an `Idempotency-Key` header. The
//! middleware stores the response (status + body) for 24 hours,
//! keyed by (caller namespace, key). A repeat request with the same
//! key replays the stored response verbatim and is marked with an
//! `X-ZIM-Idempotent-Replay: true` header; reusing the key for a
//! *different* request (method + path + query) is rejected with
//! `400 bad_request`.
//!
//! Only completed responses are stored — `429` and `5xx` outcomes
//! leave the key unset so the client may legitimately retry. The
//! fingerprint deliberately excludes the request body (§12 speaks
//! only of "a repeat request with the same key"); body-sensitive
//! replay detection can be added later without wire changes.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};

/// A stored response, ready for verbatim replay.
#[derive(Debug, Clone)]
pub struct StoredResponse {
    /// Response status code.
    pub status: u16,
    /// Response body bytes.
    pub body: Vec<u8>,
    /// Stored content type (replayed responses restore it).
    pub content_type: Option<String>,
    /// When this entry stops being valid.
    pub expires_at: DateTime<Utc>,
    /// The (method, path+query) fingerprint the key was first used
    /// with — mismatched reuse is an error, not a replay.
    pub fingerprint: String,
}

/// The (caller-scoped) idempotency store.
pub struct IdempotencyStore {
    ttl: Duration,
    entries: Mutex<HashMap<(String, String), StoredResponse>>,
}

impl IdempotencyStore {
    /// An empty store with the given retention (`ApiConfig`'s 24 h
    /// default per §12).
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// The retention window (diagnostics).
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Look up a live entry for (namespace, key).
    ///
    /// Expired entries are removed on sight and report as absent.
    pub fn lookup(&self, namespace: &str, key: &str, now: DateTime<Utc>) -> Option<StoredResponse> {
        let mut entries = self
            .entries
            .lock()
            .expect("idempotency store lock poisoned");
        match entries.get(&(namespace.to_owned(), key.to_owned())) {
            Some(stored) if stored.expires_at > now => Some(stored.clone()),
            Some(_) => {
                entries.remove(&(namespace.to_owned(), key.to_owned()));
                None
            }
            None => None,
        }
    }

    /// Store a response for (namespace, key) with the TTL's expiry.
    ///
    /// Overwrites any prior entry (the normal flow never re-stores
    /// after a hit, so an overwrite only happens after an expired
    /// entry was reaped by `lookup`).
    pub fn store(
        &self,
        namespace: &str,
        key: &str,
        fingerprint: String,
        response: StoredResponse,
        now: DateTime<Utc>,
    ) {
        let mut entries = self
            .entries
            .lock()
            .expect("idempotency store lock poisoned");
        // Amortized pruning: entries are few, but a long-lived engine
        // with many keys still stays bounded.
        if entries.len() > 4096 {
            entries.retain(|_, stored| stored.expires_at > now);
        }
        let expiry = now
            + chrono::Duration::from_std(self.ttl).unwrap_or_else(|_| chrono::Duration::hours(24));
        entries.insert(
            (namespace.to_owned(), key.to_owned()),
            StoredResponse {
                expires_at: expiry,
                fingerprint,
                ..response
            },
        );
    }

    /// Number of live entries (tests, diagnostics).
    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .expect("idempotency store lock poisoned")
            .len()
    }

    /// Whether the store holds no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_lookup_and_expiry() {
        let store = IdempotencyStore::new(Duration::from_secs(60));
        let now = Utc::now();
        store.store(
            "u1",
            "k1",
            "POST /x".into(),
            StoredResponse {
                status: 201,
                body: b"{}".to_vec(),
                content_type: None,
                expires_at: now,
                fingerprint: "POST /x".to_owned(),
            },
            now,
        );
        let hit = store.lookup("u1", "k1", now).unwrap();
        assert_eq!(hit.status, 201);
        assert_eq!(hit.fingerprint, "POST /x");

        // Namespaces are isolated.
        assert!(store.lookup("u2", "k1", now).is_none());

        // Expiry: past the TTL the entry is gone.
        let later = now + chrono::Duration::seconds(61);
        assert!(store.lookup("u1", "k1", later).is_none());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn re_store_overwrites() {
        let store = IdempotencyStore::new(Duration::from_secs(60));
        let now = Utc::now();
        store.store(
            "u",
            "k",
            "a".into(),
            StoredResponse {
                status: 200,
                body: vec![1],
                content_type: None,
                expires_at: now,
                fingerprint: "a".to_owned(),
            },
            now,
        );
        store.store(
            "u",
            "k",
            "b".into(),
            StoredResponse {
                status: 201,
                body: vec![2],
                content_type: None,
                expires_at: now,
                fingerprint: "b".to_owned(),
            },
            now,
        );
        assert_eq!(store.len(), 1);
        assert_eq!(store.lookup("u", "k", now).unwrap().status, 201);
    }
}
