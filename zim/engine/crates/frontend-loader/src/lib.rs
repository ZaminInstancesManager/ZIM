//! The engine's frontend loader (M11).
//!
//! Serves the default and custom frontend packages to the browser
//! per `docs/architecture/RUST_ENGINE.md` §10 and
//! `docs/api/FRONTEND_EXTENSION_SPEC.md`:
//!
//! - The engine stores the active frontend package's bytes and
//!   manifest in its data directory (the bundled default is
//!   compiled in and cannot be removed, only overridden).
//! - Before activating a new package the engine runs the §3.5
//!   smoke gate: manifest schema, engine compatibility, API
//!   generation, signature classification, entrypoint presence and
//!   structure, and the configured smoke page's API exercise. A
//!   failing package is refused and the previous one is kept.
//! - When the *recorded* active package fails to load or re-verify
//!   at boot, the engine reverts to the default frontend and
//!   surfaces a recovery notice (`zim://newtab`, §3.6) — invariant
//!   16: a broken frontend can never lock the user out of ZIM.
//! - The loader is wired into the engine's serving path by
//!   `zim_engine_main` (the `/zim/` routes in `zim_engine_api`),
//!   which routes HTTP paths through the active package's declared
//!   `zim://` surfaces.
//!
//! # Architecture boundary
//!
//! This crate sits below the API layer (`zim_engine_api` constructs
//! and serves it via `AppState`). It depends only on the domain and
//! application crates plus its own tooling; it never depends on the
//! active frontend's framework — the frontend is bytes and a
//! manifest, never types (spec §5).
//!
//! # Delivery notes (documented scope decisions)
//!
//! - §3.5 step 5's *execution* of the smoke page in a renderer is
//!   the browser-integration half of the gate (M13+); the engine
//!   half here statically verifies the page loads the package's API
//!   client and that the client targets the identity endpoint.
//! - The signature block is structured (`algorithm`/`public_key`/
//!   `value`, ed25519 over the canonical manifest JSON) rather than
//!   the spec sketch's opaque string; the exact packaging format
//!   was explicitly TBD (spec §7) and a self-describing block needs
//!   no out-of-band key lookup for unknown authors.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod html;
pub mod manifest;
pub mod package;
pub mod service;
pub mod signature;
pub mod smoke;

pub use manifest::{FrontendManifest, ManifestError, SignatureInfo, SurfacePattern, MANIFEST_FILE};
pub use package::{FrontendPackage, PackageError};
pub use service::{
    ActiveInfo, FrontendError, FrontendService, PackageSummary, RecoveryNotice, Resolution,
    DEFAULT_ID,
};
pub use signature::{SignatureCheck, TrustStore};
pub use smoke::{CheckResult, SmokeReport};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_public_surface_is_re_exported() {
        // The API layer consumes these names; a rename here must be
        // a conscious change, not an accident.
        let _: fn(&str) -> Option<&'static str> = service::mime_of;
        let dir = std::env::temp_dir().join(format!("zim-loader-lib-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let service = FrontendService::new(dir, semver::Version::new(0, 0, 1), "v1.0.0");
        assert!(service.active_info().is_default);
    }
}
