//! ZIM engine binary entrypoint.
//!
//! Wires the chosen concrete adapters (persistence-sqlite,
//! runtime-docker, etc.) into the application's port traits, starts
//! the HTTPS+WSS listener on loopback, and runs the engine's main
//! event loop. Per AD-4 in docs/architecture/DECISIONS.md, the engine
//! is an independent OS process separate from the browser.

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
pub const CRATE_NAME: &str = "zim_engine_main";

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
        assert_eq!(CRATE_NAME, "zim_engine_main");
    }
}
