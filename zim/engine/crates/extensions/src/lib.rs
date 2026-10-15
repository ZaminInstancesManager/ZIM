//! The engine's addon system (M24): ZIM extensions, a separate
//! ecosystem from Chromium MV3 extensions (AD-7), loaded, validated,
//! and lifecycle-managed per `docs/api/FRONTEND_EXTENSION_SPEC.md`
//! §4 and `docs/api/ADDON_SDK.md`.
//!
//! - [`manifest`] — the `zim.addon.json` schema (§4.1): identity,
//!   the closed permission vocabulary, instance scope, page
//!   surfaces, entrypoints, sidebar/context-menu/command/event
//!   contributions, dependencies, and the optional ed25519
//!   signature block.
//! - [`package`] — the `application/zip` upload format with
//!   confinement and size limits.
//! - [`signature`] — the signature classification shared with
//!   frontend packages (trusted / invalid / untrusted / unsigned)
//!   over the same trust root.
//! - [`service`] — the lifecycle (§4.11): install (dependency
//!   resolution §4.9, page-surface collision detection §4.4, no
//!   partial installs), enable, disable, update (downgrade-safe),
//!   uninstall (§6's bundled addons are disable-only), and grant
//!   (§4.2's admin-approved elevation) — plus the `/zim/` fallback
//!   routing that serves enabled addons' pages (their declared
//!   surfaces plus the reserved `zim://ext/{addon-id}/…` asset
//!   namespace), and the §4.10 error isolation: an addon whose
//!   package is corrupt on disk is disabled with a notice; the
//!   engine and every other addon keep working.
//! - [`theme`] — packaged-theme upload (M23's deferred surface):
//!   `zim.theme.json` validated against the theme-tokens.v1
//!   contract, stored in the data directory, listed for the
//!   frontend's theme panel.
//!
//! # Architecture boundary
//!
//! This crate sits below the API layer (`zim_engine_api` constructs
//! and serves it through `AppState`) next to the frontend loader,
//! and above the domain/application crates. It persists through the
//! `Persistence` port's extension family (`extension_registrations`
//! rows) and reuses the frontend loader's public primitives —
//! surface patterns, confinement-safe paths, the signature block
//! shape, and the trust store — so both package ecosystems share one
//! trust root and one routing contract.
//!
//! # Delivery notes (documented scope decisions)
//!
//! - §4.10's per-origin process isolation predates AD-20: in ZIM's
//!   post-AD-20 serving model every page is same-origin on the
//!   engine's loopback HTTPS origin, so addon isolation is
//!   disable-with-notice (the engine) plus per-pane iframe
//!   boundaries (the frontend, AD-24). Recorded as AD-29.
//! - §4.2's "checked at the engine boundary on every API call":
//!   addon pages call `/api/v1` with the same session credentials
//!   as the default frontend's pages — the engine's per-endpoint
//!   authz (invariant 15) gates every call. The declared permission
//!   set is granted, listed, revocable, and enforced client-side by
//!   the SDK; server-side per-addon attribution on the shared
//!   loopback origin is an M25 hardening item.
//! - The persisted domain manifest keeps the §4.1 core typed and
//!   preserves the complete validated manifest verbatim under the
//!   reserved `extra` key `zim_addon_manifest_v1`, so the serving
//!   index rehydrates exactly what was validated at install.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod embedded;
pub mod manifest;
pub mod package;
pub mod service;
pub mod signature;
pub mod theme;

pub use manifest::{
    AddonManifest, CommandSpec, ContextMenuItem, InstanceScope, ManifestError, SidebarItem,
    KNOWN_EVENTS, MANIFEST_FILE, MANIFEST_VERSION,
};
pub use package::{AddonPackage, PackageError, MAX_PACKAGE_BYTES, MAX_PACKAGE_FILES};
pub use service::{
    AddonSummary, ExtensionError, ExtensionsService, LoadedAddon, PermissionSummary, ServedFile,
};
pub use signature::{classify, trust_store};
pub use theme::{ThemeError, ThemeService, ThemeSummary, THEME_MANIFEST_FILE};
