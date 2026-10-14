//! Egg importer + `zim.toml` server-definition format.
//!
//! Reads existing Pterodactyl eggs (JSON) and produces ZIM egg
//! records; defines the ZIM-native `zim.toml` format preferred for
//! new definitions. Security-tight: eggs with privileged install
//! scripts (or that require root) are rejected by default per
//! `docs/compatibility/MIGRATION_AND_COMPATIBILITY.md` §3.4 — the
//! [`ImportPolicy`] carries the explicit admin opt-in.
//!
//! # Layout
//!
//! - [`legacy`] — the PTDL_v1/v2 JSON importer (lenient: unknown
//!   fields and features surface as warnings; structural violations
//!   and security rejections are errors).
//! - [`zim_toml`] — the native format, both directions (import +
//!   export; round-trip fidelity is a test invariant).
//!
//! The crate is deliberately pure: a source document in, an
//! [`Egg`] out. Persistence (`put_egg`) and the §10.11 API compose
//! it at the edges; nothing here touches the store or the network.
//!
//! # Architecture boundary
//!
//! Depends only on the domain layer. The permitted dependency
//! direction is enforced at compile time by the workspace crate
//! graph (`docs/architecture/RUST_ENGINE.md` §2).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod legacy;
pub mod zim_toml;

pub use legacy::import_legacy_json;
pub use zim_toml::{import_zim_toml, to_zim_toml};

/// The canonical crate name.
pub const CRATE_NAME: &str = "zim_engine_eggs";

/// The security policy an import runs under.
///
/// Both flags default to `false` (reject): privileged install
/// scripts and root-requiring workloads are the two legacy feature
/// flags ZIM refuses without an explicit admin opt-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImportPolicy {
    /// Allow eggs whose install scripts run privileged
    /// (`priviledged_install_scripts`, Pterodactyl's own spelling).
    pub allow_privileged_install_scripts: bool,
    /// Allow eggs whose workloads must run as root.
    pub allow_requires_root: bool,
}

/// What an import produced: the egg plus everything the importer
/// chose to tolerate instead of reject.
#[derive(Debug, Clone)]
pub struct ImportReport {
    /// The imported egg (not yet persisted).
    pub egg: zim_engine_domain::Egg,
    /// Non-fatal findings (unknown fields, skipped images,
    /// unresolved placeholders, ...).
    pub warnings: Vec<String>,
}

/// Import failures: malformed documents, structural violations,
/// and security rejections.
#[derive(Debug, thiserror::Error)]
pub enum EggImportError {
    /// The document is not valid JSON.
    #[error("the legacy egg document is malformed: {0}")]
    Json(String),
    /// The document is not a valid zim.toml file.
    #[error("the zim.toml document is malformed: {0}")]
    Toml(String),
    /// The egg has no name.
    #[error("the egg has no name")]
    MissingName,
    /// The egg has no startup command.
    #[error("the egg has no startup command")]
    MissingStartup,
    /// The egg's install scripts require privileges and the policy
    /// did not allow them.
    #[error(
        "the egg's install scripts require privileges (priviledged_install_scripts); \
         re-import with an explicit admin opt-in to accept that risk"
    )]
    PrivilegedInstallScripts,
    /// The egg's workload must run as root and the policy did not
    /// allow it.
    #[error(
        "the egg's workload must run as root (requires_root); \
         re-import with an explicit admin opt-in to accept that risk"
    )]
    RequiresRoot,
    /// A structural invariant failed.
    #[error("the egg violates a domain invariant: {0}")]
    Domain(#[from] zim_engine_domain::EggError),
}

#[cfg(test)]
mod tests {
    use super::CRATE_NAME;

    #[test]
    fn crate_name_is_set() {
        assert_eq!(CRATE_NAME, "zim_engine_eggs");
    }
}
