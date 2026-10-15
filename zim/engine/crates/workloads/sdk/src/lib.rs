//! The workload integration SDK (M16).
//!
//! The contract surface a ZIM workload integration implements, per
//! `docs/architecture/WORKLOAD_ABSTRACTION.md` (§4 the capability
//! traits, §5 the integration package manifest) and
//! `docs/architecture/RUST_ENGINE.md` §12.4 (the crate structure).
//!
//! An integration is the *kind-specific* half of workload
//! management: the engine's runtimes and supervision provide the
//! generic container/process lifecycle, and the integration
//! contributes what "a Minecraft server" or "a Postgres instance"
//! means — spec schemas, lifecycle semantics, capability
//! requirements, placement negotiation.
//!
//! First-party integrations live as feature-gated workspace crates
//! under `zim/engine/crates/workloads/`; the five M16 skeletons
//! (`minecraft`, `database`, `website`, `dev_process`,
//! `generic_container`) declare themselves with the
//! [`macro@workload_integration`] attribute. Third-party
//! integration *packages* (the dynamically-loaded form, AD-15/AD-16)
//! arrive in later milestones; the manifest parser here already
//! speaks their `zim.toml` shape.
//!
//! # Declaring an integration
//!
//! ```
//! use zim_engine_workloads_sdk::{workload_integration, WorkloadIntegration};
//!
//! #[workload_integration(
//!     kind = GameServer,
//!     id = "org.example.workload.minimal",
//!     version = "0.1.0",
//!     display_name = "Minimal Game Server",
//!     required_capabilities = [ConsoleCapable, FilesystemCapable],
//!     runtime_kinds = [Container],
//! )]
//! pub struct MinimalGameServer;
//!
//! let integration = MinimalGameServer;
//! assert_eq!(integration.kind(), zim_engine_domain::WorkloadKind::GameServer);
//! assert_eq!(integration.manifest().id, "org.example.workload.minimal");
//! assert!(integration.runtime_requirements().contains(
//!     zim_engine_workloads_sdk::Capability::ConsoleCapable,
//! ));
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod capabilities;
pub mod errors;
pub mod health;
pub mod manifest;
pub mod traits;

pub use capabilities::{Capability, CapabilitySet};
pub use errors::{IntegrationError, SpecError};
pub use health::{run_health_probe, HealthCheckKind, HealthCheckSpec};
pub use manifest::{IntegrationManifest, ManifestError, RuntimeKindRef, MANIFEST_VERSION};
pub use traits::{ArchiveFormat, ArchiveRef, Health, Readiness, WorkloadIntegration};

/// The crate name (banners and diagnostics).
pub const CRATE_NAME: &str = "zim_engine_workloads_sdk";

// The declaration macro is the macros crate's whole surface;
// re-export it so integration crates only need this crate.
pub use zim_engine_workloads_macros::workload_integration;
