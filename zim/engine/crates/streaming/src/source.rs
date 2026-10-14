//! The subscription port (RUST_ENGINE.md §6's `StreamSource`).

use crate::filter::StreamFilter;
use crate::handle::StreamHandle;
use zim_engine_domain::InstanceId;

/// Errors a subscription attempt can fail with.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubscribeError {
    /// The lane already holds its configured maximum of live
    /// subscribers (spec §13: 50 per instance by default). The
    /// client may retry after other subscribers disconnect.
    #[error("instance {instance_id:?} already has {limit} stream subscribers")]
    TooManySubscribers {
        /// The lane the subscriber wanted (`None` = the global
        /// lane).
        instance_id: Option<InstanceId>,
        /// The configured per-lane subscriber limit.
        limit: usize,
    },
}

/// The engine's stream-subscription port.
///
/// Implemented by [`crate::StreamHub`] (the M7 production source).
/// The trait exists because the RUST_ENGINE.md §6 sketch names it as
/// the seam the API layer consumes; tests and future variants (a
/// persisted event log, a remote-node relay) can substitute their
/// own implementation behind the same signature.
pub trait StreamSource: Send + Sync {
    /// Subscribe with `filter`.
    ///
    /// - Live events matching the filter are delivered on the
    ///   returned handle in per-instance generation order.
    /// - When `filter.since_generation` is `Some(n)`, events with
    ///   generation greater than `n` that are still retained are
    ///   replayed first (a `stream.gap` meta-event covers anything
    ///   evicted — see the hub module docs).
    /// - The subscription ends when the handle (and its clones) drop
    ///   or call [`StreamHandle::close`].
    ///
    /// # Recorded deviation from the §6 sketch
    ///
    /// The sketch's `subscribe(&self, filter) -> StreamHandle` has
    /// no failure mode; the spec's §13 subscriber limits require
    /// one, so the real signature returns a `Result` (the same
    /// deviation style the supervision module recorded for its
    /// async `Runtime`).
    fn subscribe(&self, filter: &StreamFilter) -> Result<StreamHandle, SubscribeError>;
}

#[cfg(test)]
mod tests {
    //! The trait carries no behavior; conformance is verified by the
    //! hub's tests. This keeps a compile-time witness that the port
    //! stays object-safe (the API layer holds it as `dyn StreamSource`
    //! when it decouples from the concrete hub).

    use super::*;

    #[test]
    fn stream_source_is_object_safe() {
        let _source: Option<&dyn StreamSource> = None;
    }
}
