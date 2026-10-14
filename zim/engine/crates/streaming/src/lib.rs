//! Console + metrics fan-out.
//!
//! # Source of truth
//!
//! This crate is the M7 realization of the streaming module specified
//! in `docs/architecture/RUST_ENGINE.md` §6 and the WebSocket event
//! contract in `docs/api/ZIM_API_SPEC.md` §11: per-instance event
//! lanes with monotonically increasing event generations, an
//! in-memory retention window (console output bounded by events and
//! bytes; state changes retained since instance creation), explicit
//! drop-oldest backpressure, and replay-from-checkpoint on
//! reconnect.
//!
//! The crate owns the fan-out from engine-internal producers
//! (supervision's lifecycle transitions, the runtime's console
//! attach pump, the jobs module's progress events) to subscribers
//! (the `/api/v1/stream` WebSocket connections). Producers call the
//! synchronous [`StreamHub::publish`] family; the API layer drives
//! subscribers with the asynchronous [`StreamHandle::recv`].
//!
//! # Event generations
//!
//! Every published event for an instance is stamped with a
//! per-instance, strictly monotonic `generation` counter owned by
//! this crate (ZIM_API_SPEC.md §11.2: events for the same
//! `instance_id` are delivered in generation order). This counter is
//! distinct from the supervision module's transition-generation
//! counter (RUST_ENGINE.md §4 invariant 13, stale-sample protection):
//! the supervision counter guards runtime-sample races internally,
//! while the event generation is the client-visible replay cursor.
//! A client reconnecting with `?since_generation={n}` — where `n` is
//! the last generation it observed on the stream — receives exactly
//! the events with generation greater than `n` that are still within
//! the retention window, a `stream.gap` meta-event covering anything
//! evicted, and then the live tail.
//!
//! # Backpressure
//!
//! A subscriber that cannot keep up must never slow the runtime: its
//! queue is bounded, and when full the oldest queued event is
//! dropped, the subscriber's `dropped_events` counter is
//! incremented, and a `stream.dropped` meta-event is inserted on the
//! next available queue slot (ZIM_API_SPEC.md §11.2).
//!
//! # Architecture boundary
//!
//! This crate's role, dependencies, and permitted callers are documented
//! in `docs/architecture/RUST_ENGINE.md` (and the expanded-scope
//! `docs/architecture/WORKLOAD_ABSTRACTION.md` where applicable).
//! The permitted dependency direction is enforced at compile time by
//! the workspace crate graph: `zim_engine_streaming` sits beside
//! `zim_engine_domain` at the bottom of the graph and depends only
//! on `zim_engine_domain` plus I/O-free utility crates — the M2
//! skeleton's accidental dependency on `zim_engine_application`
//! (inverted) was removed when this module was implemented in M7.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod event;
pub mod filter;
pub mod handle;
pub mod hub;
pub mod policy;
pub mod source;

pub use event::{EventData, EventKind, ResourceUsageData, StreamEvent, WIRE_SUBPROTOCOL};
pub use filter::{EventKindSet, StreamFilter};
pub use handle::{StreamHandle, SubscriberId};
pub use hub::StreamHub;
pub use policy::StreamingPolicy;
pub use source::{StreamSource, SubscribeError};

/// The canonical crate name, useful for runtime identification in
/// logs and crash reports.
pub const CRATE_NAME: &str = "zim_engine_streaming";

#[cfg(test)]
mod tests {
    //! Cross-module smoke tests; per-module behavior is tested in the
    //! respective modules and in the hub-level integration tests
    //! (`tests/`).

    use super::*;

    #[test]
    fn crate_name_is_set() {
        assert_eq!(CRATE_NAME, "zim_engine_streaming");
    }
}
