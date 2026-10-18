//! The history window vocabulary (M30): the five query windows the
//! metrics surface offers, their wire names, and their spans.
//!
//! A fixed vocabulary rather than free-form `since`/`until` ranges:
//! the chart's point budget, the lookback, and the downsampling
//! resolution are all derived from the window, and a bounded set
//! keeps the API's answer shape (and its tests) honest.

use std::time::Duration;

/// One history query window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricsWindow {
    /// The last 15 minutes (the live-adjacent view).
    Minutes15,
    /// The last hour (the default view).
    Hour1,
    /// The last 6 hours.
    Hours6,
    /// The last 24 hours.
    Hours24,
    /// The last 7 days (the retention ceiling by default).
    Days7,
}

impl MetricsWindow {
    /// Every supported window, in ascending span order.
    pub const ALL: [MetricsWindow; 5] = [
        MetricsWindow::Minutes15,
        MetricsWindow::Hour1,
        MetricsWindow::Hours6,
        MetricsWindow::Hours24,
        MetricsWindow::Days7,
    ];

    /// The wire name (`?window=15m`, `1h`, `6h`, `24h`, `7d`).
    pub const fn as_str(self) -> &'static str {
        match self {
            MetricsWindow::Minutes15 => "15m",
            MetricsWindow::Hour1 => "1h",
            MetricsWindow::Hours6 => "6h",
            MetricsWindow::Hours24 => "24h",
            MetricsWindow::Days7 => "7d",
        }
    }

    /// The human label (status lines and empty-state messages).
    pub const fn label(self) -> &'static str {
        match self {
            MetricsWindow::Minutes15 => "15 minutes",
            MetricsWindow::Hour1 => "hour",
            MetricsWindow::Hours6 => "6 hours",
            MetricsWindow::Hours24 => "24 hours",
            MetricsWindow::Days7 => "7 days",
        }
    }

    /// The span in seconds.
    pub const fn seconds(self) -> i64 {
        match self {
            MetricsWindow::Minutes15 => 15 * 60,
            MetricsWindow::Hour1 => 3600,
            MetricsWindow::Hours6 => 6 * 3600,
            MetricsWindow::Hours24 => 24 * 3600,
            MetricsWindow::Days7 => 7 * 24 * 3600,
        }
    }

    /// The span as a `std::time` duration (the retention task's
    /// arithmetic works in `std::time`).
    pub const fn duration(self) -> Duration {
        match self {
            MetricsWindow::Minutes15 => Duration::from_secs(15 * 60),
            MetricsWindow::Hour1 => Duration::from_secs(3600),
            MetricsWindow::Hours6 => Duration::from_secs(6 * 3600),
            MetricsWindow::Hours24 => Duration::from_secs(24 * 3600),
            MetricsWindow::Days7 => Duration::from_secs(7 * 24 * 3600),
        }
    }

    /// Parse a wire name (case-insensitive). `None` for anything
    /// else — the API layer turns that into a structured
    /// `bad_request` naming the valid vocabulary.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "15m" => Some(MetricsWindow::Minutes15),
            "1h" => Some(MetricsWindow::Hour1),
            "6h" => Some(MetricsWindow::Hours6),
            "24h" => Some(MetricsWindow::Hours24),
            "7d" => Some(MetricsWindow::Days7),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_vocabulary_round_trips() {
        for window in MetricsWindow::ALL {
            assert_eq!(MetricsWindow::parse(window.as_str()), Some(window));
        }
        // Case and surrounding whitespace are tolerated; anything
        // else is a refusal, not a guess.
        assert_eq!(MetricsWindow::parse(" 1H "), Some(MetricsWindow::Hour1));
        assert_eq!(MetricsWindow::parse("2h"), None);
        assert_eq!(MetricsWindow::parse(""), None);
        assert_eq!(MetricsWindow::parse("month"), None);
    }

    #[test]
    fn spans_are_ascending() {
        let spans: Vec<i64> = MetricsWindow::ALL.iter().map(|w| w.seconds()).collect();
        let sorted = {
            let mut copy = spans.clone();
            copy.sort_unstable();
            copy
        };
        assert_eq!(spans, sorted);
        assert_eq!(MetricsWindow::Hour1.seconds(), 3600);
        assert_eq!(MetricsWindow::Days7.seconds(), 7 * 24 * 3600);
    }

    #[test]
    fn durations_agree_with_seconds() {
        for window in MetricsWindow::ALL {
            assert_eq!(window.duration().as_secs(), window.seconds() as u64);
        }
    }
}
