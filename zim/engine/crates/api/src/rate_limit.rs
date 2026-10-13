//! In-memory sliding-window rate limiter (§9, and §15's preliminary
//! "sliding window per credential" decision).
//!
//! One limiter instance = one policy (budget + window). The API
//! server runs two: the per-credential limiter (default 600/min) and
//! the per-IP login limiter (default 10/min). Buckets are keyed by
//! an opaque string (`sess:<hash>`, `key:<id>`, `ip:<addr>`).

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};

/// The outcome of a rate-limit check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    /// Whether the request is within budget.
    pub allowed: bool,
    /// Requests remaining in the window after this one.
    pub remaining: u64,
    /// When the oldest counted request leaves the window (the retry
    /// hint / reset time).
    pub resets_at: DateTime<Utc>,
}

/// A sliding-window limiter over string keys.
pub struct SlidingWindowLimiter {
    limit: u64,
    window: Duration,
    buckets: RwLock<HashMap<String, VecDeque<DateTime<Utc>>>>,
    /// Guards the occasional full prune (cheap; amortized).
    prune_lock: Mutex<()>,
}

impl SlidingWindowLimiter {
    /// A limiter allowing `limit` events per sliding `window`.
    pub fn new(limit: u64, window: Duration) -> Self {
        Self {
            limit,
            window,
            buckets: RwLock::new(HashMap::new()),
            prune_lock: Mutex::new(()),
        }
    }

    /// The policy budget (for response headers).
    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// Count one event for `key` and decide whether it fits.
    ///
    /// The window slides: an event at time T is counted against
    /// requests in `(T - window, T]`.
    pub fn check(&self, key: &str, now: DateTime<Utc>) -> Decision {
        let cutoff = now
            - chrono::Duration::from_std(self.window).unwrap_or_else(|_| {
                // Windows beyond chrono::Duration's range clamp to its max;
                // no sane configuration lands here.
                chrono::Duration::milliseconds(i64::MAX / 4)
            });
        let mut buckets = self.buckets.write().expect("rate limiter lock poisoned");
        let bucket = buckets.entry(key.to_owned()).or_default();
        while bucket.front().is_some_and(|t| *t <= cutoff) {
            bucket.pop_front();
        }
        let allowed = bucket.len() < self.limit as usize;
        if allowed {
            bucket.push_back(now);
        }
        let resets_at = match (bucket.front(), allowed) {
            // Blocked: retry when the oldest counted request expires.
            (Some(oldest), false) => {
                *oldest
                    + chrono::Duration::from_std(self.window)
                        .unwrap_or(chrono::Duration::seconds(60))
            }
            // Allowed: window reset relevant only when full.
            (Some(oldest), true) if bucket.len() as u64 == self.limit => {
                *oldest
                    + chrono::Duration::from_std(self.window)
                        .unwrap_or(chrono::Duration::seconds(60))
            }
            _ => {
                now + chrono::Duration::from_std(self.window)
                    .unwrap_or(chrono::Duration::seconds(60))
            }
        };
        Decision {
            allowed,
            remaining: self.limit.saturating_sub(bucket.len() as u64),
            resets_at,
        }
    }

    /// Drop empty and fully-expired buckets. Called opportunistically
    /// (every ~1000 checks) so the map does not grow without bound
    /// under churn.
    pub fn prune(&self, now: DateTime<Utc>) {
        // Only one pruner at a time; others skip (their check call is
        // the expensive part anyway).
        if let Ok(_guard) = self.prune_lock.try_lock() {
            let cutoff = now - chrono::Duration::from_std(self.window).unwrap_or_default();
            let mut buckets = self.buckets.write().expect("rate limiter lock poisoned");
            buckets.retain(|_, bucket| bucket.back().is_some_and(|t| *t > cutoff));
        }
    }

    /// Number of live keys (tests, diagnostics).
    pub fn key_count(&self) -> usize {
        self.buckets
            .read()
            .expect("rate limiter lock poisoned")
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(seconds: i64) -> DateTime<Utc> {
        Utc::now() + chrono::Duration::seconds(seconds)
    }

    #[test]
    fn allows_up_to_the_limit_then_blocks() {
        let limiter = SlidingWindowLimiter::new(3, Duration::from_secs(60));
        for i in 0..3 {
            assert!(limiter.check("k", t(i)).allowed, "request {i} should pass");
        }
        let blocked = limiter.check("k", t(4));
        assert!(!blocked.allowed);
        assert_eq!(blocked.remaining, 0);
        assert!(blocked.resets_at > t(4));
    }

    #[test]
    fn window_slides_and_old_events_expire() {
        let limiter = SlidingWindowLimiter::new(2, Duration::from_secs(10));
        assert!(limiter.check("k", t(0)).allowed);
        assert!(limiter.check("k", t(1)).allowed);
        // Both slots filled; t=5 is blocked.
        assert!(!limiter.check("k", t(5)).allowed);
        // At t=11 the t=0 event has left the window (window is (t-10, t]).
        assert!(limiter.check("k", t(11)).allowed);
    }

    #[test]
    fn keys_are_independent() {
        let limiter = SlidingWindowLimiter::new(1, Duration::from_secs(60));
        assert!(limiter.check("a", t(0)).allowed);
        assert!(!limiter.check("a", t(1)).allowed);
        assert!(limiter.check("b", t(1)).allowed);
        assert_eq!(limiter.key_count(), 2);
    }

    #[test]
    fn prune_drops_fully_expired_buckets() {
        let limiter = SlidingWindowLimiter::new(1, Duration::from_secs(5));
        assert!(limiter.check("a", t(0)).allowed);
        assert_eq!(limiter.key_count(), 1);
        limiter.prune(t(100));
        assert_eq!(limiter.key_count(), 0);
    }
}
