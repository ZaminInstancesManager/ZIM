//! HTTP + WebSocket handlers for the ZIM engine API.
//!
//! Thin layer that translates HTTPS+WSS requests to application service
//! calls and enforces capability checks at the API boundary (per
//! docs/architecture/ZIM_ARCHITECTURE.md §4 invariant 15). The
//! canonical API surface is documented in docs/api/ZIM_API_SPEC.md.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![doc = ""]

//! # Architecture boundary
//!
//! This crate's role, dependencies, and permitted callers are documented
//! in `docs/architecture/RUST_ENGINE.md` (and the expanded-scope
//! `docs/architecture/WORKLOAD_ABSTRACTION.md` where applicable).
//! The permitted dependency direction is enforced at compile time by
//! the workspace crate graph.

#![allow(dead_code)]

/// Stub symbol so the crate compiles and links in M2.
///
/// Real types and implementations are introduced in milestone M3+
/// per docs/architecture/IMPLEMENTATION_ROADMAP.md.
pub const CRATE_NAME: &str = "zim_engine_api";

#[cfg(test)]
mod tests {
    //! Stub unit test so `cargo test --workspace` has something to run.
    //!
    //! Real tests are added in their corresponding implementation
    //! milestones per docs/architecture/TEST_STRATEGY.md.

    use super::CRATE_NAME;

    #[test]
    fn crate_name_is_set() {
        assert!(!CRATE_NAME.is_empty());
        assert_eq!(CRATE_NAME, "zim_engine_api");
    }
}
