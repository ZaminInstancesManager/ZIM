//! Tunables of the streaming module.
//!
//! Defaults follow `docs/api/ZIM_API_SPEC.md` §13 (limits) and
//! §11.2/§11.3 (backpressure and retention semantics). Everything is
//! in-memory per-process configuration; nothing here is persisted.

/// Configuration for the streaming hub.
#[derive(Debug, Clone)]
pub struct StreamingPolicy {
    /// Per-subscriber queue depth (in events) before the drop-oldest
    /// backpressure engages. Must be at least 2 (a meta-event and a
    /// data event must fit side by side).
    pub queue_capacity: usize,
    /// Maximum simultaneous subscribers on one instance's stream
    /// (spec §13: 50).
    pub max_subscribers_per_instance: usize,
    /// Console-output retention, in events (spec §13: 10k events
    /// per instance).
    pub console_retention_events: usize,
    /// Console-output retention, in bytes of console line payload
    /// (spec §13: 1 MB per instance).
    pub console_retention_bytes: usize,
    /// Retention for engine-global events (node state changes,
    /// audit records) — bounded because the lane is unbounded in
    /// time; a long-lived engine would otherwise grow it forever.
    /// Eviction behaves like console eviction: a `stream.gap` covers
    /// the loss on replay.
    pub global_retention_events: usize,
}

impl Default for StreamingPolicy {
    fn default() -> Self {
        Self {
            queue_capacity: 256,
            max_subscribers_per_instance: 50,
            console_retention_events: 10_000,
            console_retention_bytes: 1024 * 1024,
            global_retention_events: 10_000,
        }
    }
}

impl StreamingPolicy {
    /// Clamp nonsensical inputs to workable values (a composition
    /// root loading config from untrusted sources must not be able
    /// to configure a queue that cannot hold a meta-event plus a
    /// data event, or negative retention).
    pub fn sanitized(mut self) -> Self {
        self.queue_capacity = self.queue_capacity.max(2);
        self.console_retention_events = self.console_retention_events.max(2);
        self.console_retention_bytes = self.console_retention_bytes.max(2);
        self.global_retention_events = self.global_retention_events.max(2);
        self.max_subscribers_per_instance = self.max_subscribers_per_instance.max(1);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_spec_limits() {
        let policy = StreamingPolicy::default();
        assert_eq!(policy.max_subscribers_per_instance, 50);
        assert_eq!(policy.console_retention_events, 10_000);
        assert_eq!(policy.console_retention_bytes, 1024 * 1024);
    }

    #[test]
    fn sanitize_clamps_degenerate_values() {
        let policy = StreamingPolicy {
            queue_capacity: 0,
            max_subscribers_per_instance: 0,
            console_retention_events: 1,
            console_retention_bytes: 0,
            global_retention_events: 0,
        }
        .sanitized();
        assert!(policy.queue_capacity >= 2);
        assert!(policy.max_subscribers_per_instance >= 1);
        assert!(policy.console_retention_events >= 2);
        assert!(policy.console_retention_bytes >= 2);
        assert!(policy.global_retention_events >= 2);
    }
}
