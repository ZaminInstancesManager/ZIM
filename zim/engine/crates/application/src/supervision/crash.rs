//! Crash history and crash-loop protection (invariant 14).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};

/// The crash-loop policy: how many crashes within what window trip
/// the protection, and whether non-looped crashes auto-restart.
///
/// Defaults follow the upstream spirit: Wings refuses to auto-restart
/// a server whose last crash was less than 60 s ago
/// (`CrashDetection.Timeout = 60`); ZIM's richer form (per
/// invariant 14) counts N crashes within a window and parks the
/// instance in `crash_loop` — with the same 60 s window so a
/// boot-looping server trips after 3 rapid crashes, exactly where
/// Wings's single-window check would trip after the second one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrashLoopPolicy {
    /// How many crashes within [`Self::window`] trip the protection.
    pub threshold: u32,
    /// The counting window.
    pub window: Duration,
    /// Whether a crash that does *not* trip the loop triggers an
    /// automatic restart (Wings's `handleServerCrash` behavior).
    pub auto_restart: bool,
}

impl Default for CrashLoopPolicy {
    fn default() -> Self {
        Self {
            threshold: 3,
            window: Duration::from_secs(60),
            auto_restart: true,
        }
    }
}

/// Per-instance crash history (RUST_ENGINE.md §4's `CrashState`).
///
/// Interior-mutable so the power worker and monitors can record
/// crashes without external coordination; `in_crash_loop` is the
/// sticky flag the supervisor consults before auto-restarting.
///
/// A start command issued by a user clears the state (invariant 14's
/// "user intervention is required to clear the state" — an explicit
/// start is that intervention).
#[derive(Debug, Default)]
pub struct CrashState {
    last_crash: Mutex<Option<DateTime<Utc>>>,
    recent_crashes: Mutex<VecDeque<DateTime<Utc>>>,
    in_crash_loop: AtomicBool,
}

impl CrashState {
    /// Record a crash at `at` and return whether the loop tripped.
    ///
    /// Crashes older than the window are dropped first (the deque
    /// keeps only in-window timestamps), then the new crash is
    /// appended; if the in-window count reaches `policy.threshold`,
    /// the sticky flag is set.
    pub fn record_crash(&self, at: DateTime<Utc>, policy: &CrashLoopPolicy) -> bool {
        let window = chrono::Duration::from_std(policy.window).unwrap_or_default();
        let cutoff = at - window;

        let mut recent = self
            .recent_crashes
            .lock()
            .expect("crash state poisoned by a panicking worker");
        recent.retain(|t| *t > cutoff);
        recent.push_back(at);
        let in_window = recent.len();
        drop(recent);
        *self
            .last_crash
            .lock()
            .expect("crash state poisoned by a panicking worker") = Some(at);

        // First-trip only: a loop already engaged does not re-trip.
        let already_tripped = self.in_crash_loop.load(Ordering::Acquire);
        let tripped = in_window as u32 >= policy.threshold.max(1);
        if tripped {
            self.in_crash_loop.store(true, Ordering::Release);
        }
        tripped && !already_tripped
    }

    /// Whether the crash-loop protection is currently engaged.
    pub fn in_crash_loop(&self) -> bool {
        self.in_crash_loop.load(Ordering::Acquire)
    }

    /// The time of the most recent recorded crash, when any.
    pub fn last_crash(&self) -> Option<DateTime<Utc>> {
        *self
            .last_crash
            .lock()
            .expect("crash state poisoned by a panicking worker")
    }

    /// How many crashes are inside the current window (for state
    /// reporting and tests).
    pub fn recent_count(&self, policy: &CrashLoopPolicy) -> usize {
        let now = Utc::now();
        let window = chrono::Duration::from_std(policy.window).unwrap_or_default();
        let cutoff = now - window;
        self.recent_crashes
            .lock()
            .expect("crash state poisoned by a panicking worker")
            .iter()
            .filter(|t| **t > cutoff)
            .count()
    }

    /// Clear the crash history (a user-issued start command).
    pub fn clear(&self) {
        self.recent_crashes
            .lock()
            .expect("crash state poisoned by a panicking worker")
            .clear();
        *self
            .last_crash
            .lock()
            .expect("crash state poisoned by a panicking worker") = None;
        self.in_crash_loop.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crashes_below_threshold_do_not_trip() {
        let state = CrashState::default();
        let policy = CrashLoopPolicy {
            threshold: 3,
            window: Duration::from_secs(60),
            auto_restart: true,
        };
        let t0 = Utc::now();
        assert!(!state.record_crash(t0, &policy));
        assert!(!state.record_crash(t0, &policy));
        assert!(!state.in_crash_loop());
        assert_eq!(state.recent_count(&policy), 2);
    }

    #[test]
    fn threshold_trips_the_sticky_flag() {
        let state = CrashState::default();
        let policy = CrashLoopPolicy {
            threshold: 3,
            window: Duration::from_secs(60),
            auto_restart: true,
        };
        let t0 = Utc::now();
        state.record_crash(t0, &policy);
        state.record_crash(t0, &policy);
        assert!(state.record_crash(t0, &policy));
        assert!(state.in_crash_loop());
        // Sticky: further records do not un-trip it.
        assert!(!state.record_crash(t0, &policy));
        assert!(state.in_crash_loop());
    }

    #[test]
    fn old_crashes_age_out_of_the_window() {
        let state = CrashState::default();
        let policy = CrashLoopPolicy {
            threshold: 3,
            window: Duration::from_secs(60),
            auto_restart: true,
        };
        let t0 = Utc::now();
        // Two crashes, then a long gap, then two more: the window
        // only ever holds two.
        state.record_crash(t0, &policy);
        state.record_crash(t0, &policy);
        let later = t0 + chrono::Duration::seconds(120);
        state.record_crash(later, &policy);
        state.record_crash(later, &policy);
        assert!(!state.in_crash_loop());
        assert_eq!(state.recent_count(&policy), 2);
        assert_eq!(state.last_crash(), Some(later));
    }

    #[test]
    fn clearing_resets_everything() {
        let state = CrashState::default();
        let policy = CrashLoopPolicy::default();
        let t0 = Utc::now();
        state.record_crash(t0, &policy);
        state.record_crash(t0, &policy);
        state.record_crash(t0, &policy);
        assert!(state.in_crash_loop());
        state.clear();
        assert!(!state.in_crash_loop());
        assert_eq!(state.recent_count(&policy), 0);
        assert_eq!(state.last_crash(), None);
    }
}
