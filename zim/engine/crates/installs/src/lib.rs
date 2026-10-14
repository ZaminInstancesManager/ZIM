//! Install (and update) jobs: the executable half of M8.3 — egg
//! scripts in constrained containers.
//!
//! # What lives here
//!
//! The last M8 family: [`JobKind::Install`] (and its sibling
//! [`JobKind::Update`]) driven by the same durable-job executor as
//! backups and transfers. Like those crates this one has two
//! halves:
//!
//! - [`InstallService`] — the API-facing half: validation (the
//!   instance and its egg exist; no other install is in flight),
//!   the §10.3 `POST /instances/{id}/install` mutation family,
//!   and job creation. `M8.3` retires that endpoint's `501`.
//! - [`InstallJobHandler`] — the executable half: stop the
//!   workload if it is running, pull the egg's installer image,
//!   run the verbatim script in a constrained container bound to
//!   the instance's data directory (`InstallRunner` port), and
//!   stamp `installed_at` on success.
//!
//! # Semantics (Pterodactyl reinstall parity)
//!
//! - **Stop-then-run.** A running workload is stopped through the
//!   supervision power queue first (invariant 9's serialization:
//!   the install's stop lines up behind whatever the queue is
//!   already doing).
//! - **No auto-restart.** Pterodactyl's reinstall leaves the
//!   server stopped when the script finishes; the operator starts
//!   it. ZIM keeps that contract (a transfer *restores* power
//!   state because its purpose is to move a running workload; an
//!   install's purpose is to replace the data a restart would
//!   use).
//! - **Partial data is retry state.** A failed script leaves its
//!   partial downloads in the data directory (the next install
//!   re-runs over them — the legacy scripts are written for that);
//!   the job answers `Failed` with `cleanup_pending = false`
//!   because the "partial state" belongs to the instance, not to
//!   the job. The installer *container* itself never outlives its
//!   run (the Docker adapter removes it on every exit path).
//! - **`skip_egg_scripts`.** An instance flagged
//!   [`Instance::skip_egg_scripts`] (or an explicit request body
//!   override) skips the script and just stamps `installed_at` —
//!   the "pre-installed data" escape hatch.
//! - **Update jobs** run the egg's update script when it has one,
//!   falling back to the install script (Pterodactyl's
//!   egg-change reinstall).
//!
//! # Architecture boundary
//!
//! Depends on the domain, application (the `InstallRunner` port +
//! supervision), and jobs layers — exactly the shape
//! `docs/architecture/RUST_ENGINE.md` §2 prescribes for the M8
//! handler crates. The Docker adapter that implements the port
//! lives in `zim_engine_runtime_docker`; this crate never depends
//! on it (the composition root wires them together).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod handler;
mod service;

pub use handler::{InstallJobHandler, InstallPhasePolicy};
pub use service::{InstallError, InstallOptions, InstallService};

/// The canonical crate name.
pub const CRATE_NAME: &str = "zim_engine_installs";
