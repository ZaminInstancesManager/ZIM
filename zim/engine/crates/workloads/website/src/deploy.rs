//! The website deploy path (M19): the release-directory layout
//! and the documented deploy plan.
//!
//! The §M19 deploy lifecycle is "new version deployed alongside
//! old, drain old, switch" — the blue-green shape, realized over
//! the instance's bind-mounted data directory:
//!
//! ```text
//! <data dir>/
//! ├── nginx.conf              # the static carrier's engine-written config
//! ├── releases/
//! │   ├── 8f14e45f/           # one directory per release
//! │   │   └── site/           # the site tree (Static) / the app tree
//! │   └── c2541ff6/
//! └── current -> releases/c2541ff6   # the atomically-switched symlink
//! ```
//!
//! - **Alongside:** a new release is written into its own
//!   directory while the current one keeps serving — the server's
//!   document root resolves through `current` per request, so the
//!   write is invisible until the switch.
//! - **Switch:** `current` is swapped atomically (a temp symlink +
//!   `rename(2)` over the old link — the rename replaces the link
//!   in one step; there is no window where `current` is absent).
//! - **Drain:** for a static carrier there is nothing to drain —
//!   in-flight requests finish against the old release's already
//!   open file handles, and the next request resolves the new
//!   link. For the application servers the drain step is the
//!   restart that reloads the entrypoint (the release's process
//!   must be relaunched to serve new code; the plan says so).
//! - **Retain:** the previous releases stay on disk (bounded by
//!   the retention count) so [`rollback`][crate::WebsiteWorkload]
//!   is a symlink flip, not a re-upload.
//!
//! The executable surface lives on
//! [`WebsiteWorkload`][crate::WebsiteWorkload]
//! ([`deploy_async`][crate::WebsiteWorkload::deploy_async] /
//! [`rollback_async`][crate::WebsiteWorkload::rollback_async]);
//! this module carries the layout vocabulary and the documented
//! plan the API surfaces as "what will happen".

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use serde::{Deserialize, Serialize};

use crate::spec::WebsiteSpec;

/// The releases directory, relative to the instance data root.
pub const RELEASES_DIR: &str = "releases";

/// The current-release symlink's name, relative to the instance
/// data root.
pub const CURRENT_LINK: &str = "current";

/// The subdirectory inside one release carrying the site tree
/// (the static carrier's document root points inside it, which
/// keeps the release's own metadata — none today, but the layout
/// does not paint us into a corner — out of the served tree).
pub const SITE_SUBDIR: &str = "site";

/// The default release retention (how many old releases stay
/// on disk after a deploy — the rollback window).
pub const DEFAULT_RETAINED_RELEASES: usize = 3;

/// One step of the documented deploy plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployStep {
    /// The step's ordinal position.
    pub step: u8,
    /// The step's name.
    pub name: String,
    /// What happens.
    pub detail: String,
}

/// The documented §M19 deploy plan for one spec.
///
/// This is the "what will happen" payload the API surfaces; the
/// executable form is
/// [`WebsiteWorkload::deploy_async`][crate::WebsiteWorkload::deploy_async].
pub fn deploy_plan(spec: &WebsiteSpec) -> Vec<DeployStep> {
    let retain = spec.retained_releases();
    let alongside = match spec.server {
        crate::server::WebServer::Static => {
            "the new release's files are written into its own \
             releases/<id>/ directory while the current release \
             keeps serving (nginx resolves the current symlink per \
             request, so the write is invisible until the switch)"
        }
        crate::server::WebServer::Node | crate::server::WebServer::Python => {
            "the new release's files are written into its own \
             releases/<id>/ directory alongside the old one; the \
             running application keeps serving old code until the \
             relaunch"
        }
    };
    let switch = match spec.server {
        crate::server::WebServer::Static => {
            "the current symlink is renamed onto the new release \
             (atomic); the next request serves the new files"
        }
        crate::server::WebServer::Node | crate::server::WebServer::Python => {
            "the current symlink is renamed onto the new release \
             (atomic) and the application is relaunched so the new \
             entrypoint code loads"
        }
    };
    let drain = match spec.server {
        crate::server::WebServer::Static => {
            "in-flight requests finish against the old release's \
             open file handles; there are no long-lived \
             connections to drain"
        }
        crate::server::WebServer::Node | crate::server::WebServer::Python => {
            "the old release's process exits with the relaunch \
             (graceful stop first, the forcible backstop after \
             the deadline)"
        }
    };
    vec![
        DeployStep {
            step: 1,
            name: "upload".to_owned(),
            detail: format!(
                "the new version's files are written under \
                 {RELEASES_DIR}/<id>/{SITE_SUBDIR}/ inside the \
                 workload's data directory"
            ),
        },
        DeployStep {
            step: 2,
            name: "alongside".to_owned(),
            detail: alongside.to_owned(),
        },
        DeployStep {
            step: 3,
            name: "switch".to_owned(),
            detail: switch.to_owned(),
        },
        DeployStep {
            step: 4,
            name: "drain".to_owned(),
            detail: drain.to_owned(),
        },
        DeployStep {
            step: 5,
            name: "verify".to_owned(),
            detail: "the health check probes the live endpoint; a failed \
                 verdict fails the deploy (the rollback window stays \
                 open)"
                .to_owned(),
        },
        DeployStep {
            step: 6,
            name: "retain".to_owned(),
            detail: format!(
                "the {retain} previous releases stay on disk (the \
                 rollback window); older ones are deleted"
            ),
        },
    ]
}

/// The documented rollback plan (one step: the flip back).
pub fn rollback_plan() -> Vec<DeployStep> {
    vec![DeployStep {
        step: 1,
        name: "switch-back".to_owned(),
        detail: "the current symlink is renamed back onto the previous \
             release; the drain and retain semantics are the deploy's \
             own, in reverse"
            .to_owned(),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::default_spec;

    #[test]
    fn the_deploy_steps_are_the_alongside_switch_drain_family() {
        let plan = deploy_plan(&default_spec());
        let names: Vec<&str> = plan.iter().map(|step| step.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["upload", "alongside", "switch", "drain", "verify", "retain"]
        );
        assert_eq!(plan[0].step, 1);
        assert_eq!(plan[5].step, 6);
        // The static plan names its own specifics.
        assert!(plan[1].detail.contains("releases/<id>"));
        assert!(plan[2].detail.contains("atomic"));
        assert!(plan[3].detail.contains("open file handles"));
        assert!(plan[5].detail.contains("3 previous releases"));
    }

    #[test]
    fn the_application_servers_relaunch_on_switch() {
        let spec = crate::spec::WebsiteSpec {
            server: crate::server::WebServer::Node,
            ..default_spec()
        };
        let plan = deploy_plan(&spec);
        assert!(plan[2].detail.contains("relaunched"));
        assert!(plan[3].detail.contains("forcible backstop"));
    }

    #[test]
    fn the_rollback_plan_is_the_flip_back() {
        let plan = rollback_plan();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].name, "switch-back");
        assert!(plan[0].detail.contains("previous release"));
    }

    #[test]
    fn the_plans_round_trip_through_serde() {
        let plan = deploy_plan(&default_spec());
        let json = serde_json::to_value(&plan).unwrap();
        let back: Vec<DeployStep> = serde_json::from_value(json).unwrap();
        assert_eq!(back, plan);
    }
}
