//! The console retention buffer (M17).
//!
//! The workload handle's console pump (the task consuming the
//! runtime's [`ConsoleAttachment`]) pushes every decoded output
//! line here; the [`ConsoleCapable::console_tail`] implementation
//! reads from it. Retention is bounded with drop-oldest semantics
//! and a lost-line counter — the same backpressure philosophy the
//! streaming hub applies (M7), applied integration-side so the
//! `console_tail` window survives without a live WSS subscriber.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// The default retention bound (lines) — sized like the streaming
/// hub's console replay window.
pub const DEFAULT_CONSOLE_CAPACITY: usize = 5000;

/// A bounded, thread-safe console line buffer.
#[derive(Debug)]
pub struct ConsoleBuffer {
    lines: Mutex<VecDeque<String>>,
    capacity: usize,
    lost: AtomicU64,
}

impl ConsoleBuffer {
    /// A buffer with the default capacity.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CONSOLE_CAPACITY)
    }

    /// A buffer with an explicit capacity.
    ///
    /// A zero capacity is normalized to one (the buffer always
    /// retains the most recent line — `console_tail(1)` is the
    /// minimum useful window).
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            lines: Mutex::new(VecDeque::new()),
            capacity: capacity.max(1),
            lost: AtomicU64::new(0),
        }
    }

    /// Push one decoded output line; the oldest line is dropped
    /// when the bound is reached.
    pub fn push_line(&self, line: impl Into<String>) {
        let mut lines = self.lines.lock().expect("console buffer lock");
        if lines.len() >= self.capacity {
            lines.pop_front();
            self.lost.fetch_add(1, Ordering::Relaxed);
        }
        lines.push_back(line.into());
    }

    /// The most recent `lines` lines, oldest first.
    ///
    /// More than retained lines answers everything retained.
    pub fn tail(&self, lines: usize) -> Vec<String> {
        let buffer = self.lines.lock().expect("console buffer lock");
        let take = lines.min(buffer.len());
        buffer.iter().rev().take(take).rev().cloned().collect()
    }

    /// The number of retained lines.
    pub fn len(&self) -> usize {
        self.lines.lock().expect("console buffer lock").len()
    }

    /// Whether no lines are retained.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many lines were dropped by the bound (the backpressure
    /// counter — surfaced so callers can tell a short `tail` from
    /// an overflowed one).
    pub fn lost_lines(&self) -> u64 {
        self.lost.load(Ordering::Relaxed)
    }

    /// The configured capacity.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Drop every retained line (a new run of the workload starts a
    /// fresh console history).
    pub fn clear(&self) {
        self.lines.lock().expect("console buffer lock").clear();
    }
}

impl Default for ConsoleBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_returns_the_most_recent_lines_oldest_first() {
        let buffer = ConsoleBuffer::with_capacity(10);
        for i in 0..5 {
            buffer.push_line(format!("line-{i}"));
        }
        assert_eq!(buffer.len(), 5);
        assert_eq!(
            buffer.tail(3),
            vec![
                "line-2".to_owned(),
                "line-3".to_owned(),
                "line-4".to_owned()
            ]
        );
        assert_eq!(buffer.tail(100).len(), 5);
    }

    #[test]
    fn the_bound_drops_oldest_and_counts_the_loss() {
        let buffer = ConsoleBuffer::with_capacity(3);
        for i in 0..7 {
            buffer.push_line(format!("line-{i}"));
        }
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.lost_lines(), 4);
        assert_eq!(
            buffer.tail(3),
            vec![
                "line-4".to_owned(),
                "line-5".to_owned(),
                "line-6".to_owned()
            ]
        );
    }

    #[test]
    fn a_zero_capacity_is_normalized_to_one() {
        let buffer = ConsoleBuffer::with_capacity(0);
        buffer.push_line("only");
        buffer.push_line("latest");
        assert_eq!(buffer.capacity(), 1);
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.tail(1), vec!["latest".to_owned()]);
        assert_eq!(buffer.lost_lines(), 1);
    }

    #[test]
    fn clear_resets_the_history_but_not_the_counter() {
        let buffer = ConsoleBuffer::with_capacity(2);
        buffer.push_line("a");
        buffer.push_line("b");
        buffer.push_line("c"); // lost: 1
        buffer.clear();
        assert!(buffer.is_empty());
        assert_eq!(buffer.lost_lines(), 1);
        buffer.push_line("fresh");
        assert_eq!(buffer.tail(1), vec!["fresh".to_owned()]);
    }

    #[test]
    fn the_default_capacity_matches_the_replay_window() {
        assert_eq!(DEFAULT_CONSOLE_CAPACITY, 5000);
        assert_eq!(ConsoleBuffer::new().capacity(), 5000);
    }
}
