//! The Website install path (M19): the first-run plan and the
//! static carrier's configuration writer.
//!
//! A website's first run needs the release structure
//! bootstrapped: `releases/<id>/site/` carrying a placeholder
//! page, the `current` symlink pointed at it, and — for the
//! static carrier — the engine-written `nginx.conf` the server's
//! argv references (see [`WebServer::default_command`]). The
//! executable form is
//! [`WebsiteWorkload::install_async`][crate::WebsiteWorkload::install_async];
//! this module carries the documented plan and the configuration
//! text itself, so the plan and the bytes it describes live in
//! one place.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use serde::{Deserialize, Serialize};

use crate::deploy::SITE_SUBDIR;
use crate::spec::WebsiteSpec;

/// The placeholder page the first release serves (the install's
/// own "it works" page — the site's real content arrives with the
/// first deploy).
pub const PLACEHOLDER_INDEX: &str = "<!doctype html>\n<html>\n<head><meta charset=\"utf-8\">\n<title>ZIM website</title></head>\n<body>\n<h1>It works — managed by ZIM.</h1>\n<p>This placeholder page was written by the website install step.\nDeploy your site's files to replace it.</p>\n</body>\n</html>\n";

/// One step of the documented install plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallStep {
    /// The step's ordinal position.
    pub step: u8,
    /// The step's name.
    pub name: String,
    /// What happens.
    pub detail: String,
}

/// The documented first-run install plan for one spec.
pub fn build_install_plan(spec: &WebsiteSpec) -> Vec<InstallStep> {
    let mut steps = Vec::new();
    if matches!(spec.server, crate::server::WebServer::Static) {
        steps.push(InstallStep {
            step: 1,
            name: "write-config".to_owned(),
            detail: format!(
                "the engine-written nginx.conf lands at the data root \
                 ({}): one server block, the document root pointed at \
                 the current release, access logging to stdout",
                crate::server::WebServer::CONFIG_PATH
            ),
        });
    }
    let next = steps.len() as u8 + 1;
    steps.push(InstallStep {
        step: next,
        name: "first-release".to_owned(),
        detail: format!(
            "the first release is written under \
             {}/{SITE_SUBDIR}/ with a placeholder page (the site's \
             real content arrives with the first deploy)",
            crate::deploy::RELEASES_DIR
        ),
    });
    let next = steps.len() as u8 + 1;
    steps.push(InstallStep {
        step: next,
        name: "switch-current".to_owned(),
        detail: format!(
            "the {} symlink is pointed at the first release (the \
             server's document root resolves it per request)",
            crate::deploy::CURRENT_LINK
        ),
    });
    if let Some(entrypoint) = spec.server.release_entrypoint() {
        steps.push(InstallStep {
            step: steps.len() as u8 + 1,
            name: "verify-entrypoint".to_owned(),
            detail: format!(
                "the {entrypoint} entrypoint is expected in the \
                 release (the application server's argv references it; \
                 a missing entrypoint surfaces as the health check's \
                 Unhealthy verdict, not a silent hang)"
            ),
        });
    }
    steps
}

/// The static carrier's configuration text for one spec.
///
/// One `http` block, one `server` block: the document root at
/// the current release's site tree, the same index conventions
/// nginx's stock configuration carries, and the logs on the
/// container's stdout/stderr (the standard container posture —
/// `docker logs` and the console stream see the access log).
pub fn nginx_conf(spec: &WebsiteSpec) -> String {
    format!(
        "# engine-written by the ZIM website install step (M19); edits \
         are overwritten by the next install\n\
         pid /var/run/nginx.pid;\n\
         error_log /dev/stderr notice;\n\
         worker_processes auto;\n\
         events {{ worker_connections 64; }}\n\
         http {{\n\
         \tinclude /etc/nginx/mime.types;\n\
         \tdefault_type application/octet-stream;\n\
         \taccess_log /dev/stdout;\n\
         \tsendfile on;\n\
         \tserver {{\n\
         \t\tlisten {};\n\
         \t\troot {};\n\
         \t\tindex {} {} {};\n\
         \t}}\n\
         }}\n",
        spec.primary_container_port(),
        spec.server.docroot(),
        "index.html",
        "index.htm",
        "index.txt",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::default_spec;

    #[test]
    fn the_static_install_plan_writes_config_release_and_link() {
        let spec = default_spec();
        let plan = build_install_plan(&spec);
        let names: Vec<&str> = plan.iter().map(|step| step.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["write-config", "first-release", "switch-current"]
        );
        assert!(plan[0].detail.contains("nginx.conf"));
        assert!(plan[1].detail.contains("placeholder"));
        assert!(plan[2].detail.contains("current"));
    }

    #[test]
    fn the_application_servers_verify_their_entrypoints() {
        let spec = crate::spec::WebsiteSpec {
            server: crate::server::WebServer::Node,
            ..default_spec()
        };
        let plan = build_install_plan(&spec);
        let names: Vec<&str> = plan.iter().map(|step| step.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["first-release", "switch-current", "verify-entrypoint"]
        );
        assert!(plan[2].detail.contains("server.js"));
    }

    #[test]
    fn the_nginx_conf_points_at_the_current_release() {
        let conf = nginx_conf(&default_spec());
        assert!(conf.contains("listen 80;"));
        assert!(conf.contains("root /home/container/current/site;"));
        assert!(conf.contains("index index.html"));
        assert!(conf.contains("access_log /dev/stdout"));
        assert!(conf.contains("error_log /dev/stderr"));
        // The mime types ride the image's own file.
        assert!(conf.contains("include /etc/nginx/mime.types;"));
    }

    #[test]
    fn the_plans_round_trip_through_serde() {
        let plan = build_install_plan(&default_spec());
        let json = serde_json::to_value(&plan).unwrap();
        let back: Vec<InstallStep> = serde_json::from_value(json).unwrap();
        assert_eq!(back, plan);
    }
}
