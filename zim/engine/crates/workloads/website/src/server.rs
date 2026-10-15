//! The website server table (M19).
//!
//! [`WebServer`] is the Website-kind equivalent of the Database
//! kind's engine table (M18): the per-server conventions a
//! `WebsiteSpec`'s defaults come from — the image, the logical
//! endpoints, the document-root posture, and the server argv.
//!
//! Three server classes ship (the roadmap's §M19 vocabulary:
//! "nginx-config / static / node / python", with the nginx-config
//! and static forms sharing the nginx carrier):
//!
//! - [`WebServer::Static`] — nginx serving a document root. No
//!   application process; the release-switch deploy (see
//!   [`crate::deploy`]) is atomic from the server's point of
//!   view.
//! - [`WebServer::Node`] — a Node application whose entrypoint is
//!   the release's `server.js`.
//! - [`WebServer::Python`] — a Python application whose entrypoint
//!   is the release's `server.py`.
//!
//! Every class carries the same release-directory convention:
//! the site's files live under `releases/<id>/` inside the
//! instance's data directory, and the container's document root
//! points at the `current` symlink the deploy path switches.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use zim_engine_workloads_sdk::errors::SpecError;

use crate::spec::SpecEndpoint;

/// The logical endpoint name every server's primary HTTP listener
/// carries.
pub const HTTP_ENDPOINT: &str = "http";

/// The logical endpoint name the TLS listener carries (bound when
/// the spec's `https` block enables it; M19 serves the listener,
/// with the engine's certificate surface arriving with the
/// engine's own TLS milestone).
pub const HTTPS_ENDPOINT: &str = "https";

/// The server class a website runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebServer {
    /// nginx serving a static document root.
    Static,
    /// A Node application (`node <release>/server.js`).
    Node,
    /// A Python application (`python <release>/server.py`).
    Python,
}

impl WebServer {
    /// Every server class, in declaration order.
    pub const ALL: [WebServer; 3] = [WebServer::Static, WebServer::Node, WebServer::Python];

    /// The server's default image.
    pub fn default_image(self) -> &'static str {
        match self {
            Self::Static => "nginx:1.27-alpine",
            Self::Node => "node:22-alpine",
            Self::Python => "python:3.12-alpine",
        }
    }

    /// The server's default logical endpoints (the name, the
    /// container-side port, and whether the endpoint is bound by
    /// default).
    ///
    /// Every server speaks HTTP on its own conventional port; the
    /// HTTPS listener rides the same application process (Static's
    /// nginx terminates it in-config; Node/Python applications
    /// bind it themselves when their spec enables HTTPS).
    pub fn default_endpoints(self) -> Vec<SpecEndpoint> {
        let http = match self {
            Self::Static => 80,
            Self::Node => 3000,
            Self::Python => 8000,
        };
        vec![
            SpecEndpoint::new(HTTP_ENDPOINT, http),
            SpecEndpoint::new(HTTPS_ENDPOINT, 443),
        ]
    }

    /// The server's default argv.
    ///
    /// Static runs nginx under the engine-written configuration
    /// (see [`Self::config_relative_path`]); the application
    /// servers run their release entrypoints.
    pub fn default_command(self) -> Vec<String> {
        match self {
            Self::Static => vec![
                "nginx".to_owned(),
                "-c".to_owned(),
                format!("/home/container/{}", Self::CONFIG_PATH),
                "-g".to_owned(),
                "daemon off;".to_owned(),
            ],
            Self::Node => vec![
                "node".to_owned(),
                format!("/home/container/{}/server.js", crate::deploy::CURRENT_LINK),
            ],
            Self::Python => vec![
                "python".to_owned(),
                format!("/home/container/{}/server.py", crate::deploy::CURRENT_LINK),
            ],
        }
    }

    /// The container-side path the static server's document root
    /// points at — always the current release, so the deploy
    /// switch is picked up per request.
    pub fn docroot(self) -> String {
        match self {
            Self::Static => format!(
                "/home/container/{}/{}",
                crate::deploy::CURRENT_LINK,
                crate::deploy::SITE_SUBDIR
            ),
            Self::Node | Self::Python => format!("/home/container/{}", crate::deploy::CURRENT_LINK),
        }
    }

    /// The server's release entrypoint filename (the file the
    /// application servers execute out of the current release).
    pub fn release_entrypoint(self) -> Option<&'static str> {
        match self {
            Self::Static => None,
            Self::Node => Some("server.js"),
            Self::Python => Some("server.py"),
        }
    }

    /// The instance data directory's root for the server
    /// configuration file (the static class only).
    pub const CONFIG_PATH: &'static str = "nginx.conf";

    /// The server configuration file's path relative to the
    /// instance data directory root (the static class only).
    pub fn config_relative_path(self) -> &'static str {
        Self::CONFIG_PATH
    }

    /// Whether the server's container needs a writable root
    /// filesystem.
    ///
    /// The static carrier (nginx) writes its pid and cache under
    /// `/var/cache` and `/var/run` — the same posture the MySQL
    /// family needed in M18. The application servers write only
    /// their bind-mounted data directory (plus, for Node/Python
    /// dev-shaped images, their own caches under `$HOME`, which
    /// the default non-root user owns).
    pub fn writable_rootfs(self) -> bool {
        matches!(self, Self::Static)
    }

    /// The server's container user override (`None` keeps the
    /// runtime's global default; the static carrier runs its
    /// master as root the way the database engines did).
    pub fn container_user(self) -> Option<&'static str> {
        match self {
            Self::Static => Some("0:0"),
            Self::Node | Self::Python => None,
        }
    }

    /// The human name (diagnostics).
    pub fn display(self) -> &'static str {
        match self {
            Self::Static => "static (nginx)",
            Self::Node => "node",
            Self::Python => "python",
        }
    }
}

impl FromStr for WebServer {
    type Err = SpecError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "static" => Ok(Self::Static),
            "node" => Ok(Self::Node),
            "python" => Ok(Self::Python),
            other => Err(SpecError::Unparsable(format!(
                "{other:?} is not a website server class (static, node, python)"
            ))),
        }
    }
}

impl std::fmt::Display for WebServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Static => "static",
            Self::Node => "node",
            Self::Python => "python",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_server_class_carries_its_conventions() {
        for server in WebServer::ALL {
            assert!(!server.default_image().is_empty(), "{server}");
            assert!(
                server.default_image().starts_with(match server {
                    WebServer::Static => "nginx",
                    WebServer::Node => "node",
                    WebServer::Python => "python",
                }),
                "{server}"
            );
            let endpoints = server.default_endpoints();
            assert_eq!(endpoints[0].name, HTTP_ENDPOINT, "{server}");
            assert!(endpoints.iter().any(|e| e.name == HTTPS_ENDPOINT), "{server}");
            assert!(!server.default_command().is_empty(), "{server}");
            assert!(server.docroot().contains(crate::deploy::CURRENT_LINK), "{server}");
        }
    }

    #[test]
    fn the_static_server_runs_the_engine_written_config() {
        let command = WebServer::Static.default_command();
        assert_eq!(command[0], "nginx");
        assert!(command.iter().any(|arg| arg.ends_with("nginx.conf")));
        assert_eq!(WebServer::Static.config_relative_path(), "nginx.conf");
        // The static carrier needs the writable-rootfs posture.
        assert!(WebServer::Static.writable_rootfs());
        assert_eq!(WebServer::Static.container_user(), Some("0:0"));
        assert_eq!(WebServer::Static.release_entrypoint(), None);
    }

    #[test]
    fn the_application_servers_run_their_release_entrypoints() {
        let command = WebServer::Node.default_command();
        assert_eq!(command[0], "node");
        assert!(command[1].ends_with("/current/server.js"));
        assert_eq!(WebServer::Node.release_entrypoint(), Some("server.js"));
        assert!(!WebServer::Node.writable_rootfs());
        assert!(WebServer::Node.container_user().is_none());

        let command = WebServer::Python.default_command();
        assert_eq!(command[0], "python");
        assert!(command[1].ends_with("/current/server.py"));
    }

    #[test]
    fn the_class_parses_and_displays() {
        assert_eq!("static".parse::<WebServer>().unwrap(), WebServer::Static);
        assert_eq!("node".parse::<WebServer>().unwrap(), WebServer::Node);
        assert_eq!("python".parse::<WebServer>().unwrap(), WebServer::Python);
        assert!("nginx".parse::<WebServer>().is_err());
        assert_eq!(WebServer::Python.to_string(), "python");
        assert_eq!(WebServer::Static.display(), "static (nginx)");
    }

    #[test]
    fn the_class_round_trips_through_serde() {
        let json = serde_json::to_value(WebServer::Node).unwrap();
        assert_eq!(json, serde_json::json!("node"));
        let back: WebServer = serde_json::from_value(json).unwrap();
        assert_eq!(back, WebServer::Node);
    }
}
