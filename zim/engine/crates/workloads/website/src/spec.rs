//! The Website workload spec (M19).
//!
//! Per `docs/architecture/WORKLOAD_ABSTRACTION.md` §3, a
//! `Workload` row of kind `Website` carries this spec as its
//! `spec` JSON. The server table ([`WebServer`][crate::server])
//! contributes the per-class conventions; the spec carries the
//! user's decisions on top — image override, endpoints, the
//! health check, the HTTPS posture, and the deploy retention.
//!
//! The spec's *resolved* forms are the instance-facing halves:
//! [`WebsiteSpec::effective_image`],
//! [`WebsiteSpec::resolved_env`], and
//! [`WebsiteSpec::resolved_command`] build the runtime's
//! `InstanceConfig` fields the way the Database spec's do (M18).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use zim_engine_domain::DockerImage;
use zim_engine_workloads_sdk::errors::SpecError;
use zim_engine_workloads_sdk::HealthCheckSpec;

use crate::deploy::DEFAULT_RETAINED_RELEASES;
use crate::server::{HTTPS_ENDPOINT, HTTP_ENDPOINT, WebServer};

/// The runtime-facing container posture the static carrier needs
/// (the same per-instance overrides the Database kind introduced
/// in M18, carried here for the nginx carrier's pid/cache
/// writes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerPosture {
    /// The container user (`uid:gid` or name).
    pub user: Option<String>,
    /// Whether the container keeps the engine's hardened
    /// read-only root filesystem.
    pub readonly_rootfs: bool,
}

/// One logical endpoint the website workload exposes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecEndpoint {
    /// The endpoint's logical name (`"http"`, `"https"`, ...).
    pub name: String,
    /// The container-side port the server listens on.
    pub container_port: u16,
}

impl SpecEndpoint {
    /// Construct one endpoint declaration.
    pub fn new(name: &str, container_port: u16) -> Self {
        Self {
            name: name.to_owned(),
            container_port,
        }
    }
}

/// The HTTPS posture: whether the TLS listener is bound.
///
/// M19 binds the listener (the endpoint is exposed and
/// connectable); certificate management is the engine's TLS
/// milestone's surface, recorded here as the spec's
/// `certificate_ref` string (the secrets-store reference the
/// future surface resolves — never an inline key).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HttpsSpec {
    /// Whether the TLS listener is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// The secrets-store reference for the certificate (the
    /// engine's TLS surface resolves it; the spec never carries
    /// key material inline).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_ref: Option<String>,
}

/// The Website workload spec.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebsiteSpec {
    /// The server class (drives every default below).
    pub server: WebServer,
    /// The image override (default: the server table's image).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// The server version label (diagnostics only; the image
    /// reference is the source of truth).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The logical endpoints (default: the server table's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoints: Option<Vec<SpecEndpoint>>,
    /// The health check (default: an HTTP probe against the
    /// primary endpoint's root).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_check: Option<HealthCheckSpec>,
    /// The HTTPS posture.
    #[serde(default)]
    pub https: HttpsSpec,
    /// How many previous releases stay on disk after a deploy
    /// (the rollback window; default
    /// [`DEFAULT_RETAINED_RELEASES`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_releases: Option<usize>,
    /// Extra server args appended after the server's default argv
    /// (e.g. nginx `-g worker_processes 2;`, node `--inspect`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,
}

impl WebsiteSpec {
    /// The spec's effective image (the override or the server
    /// table's default).
    pub fn effective_image(&self) -> &str {
        self.image.as_deref().unwrap_or(self.server.default_image())
    }

    /// The spec's effective endpoints (the override or the
    /// server table's defaults, with the HTTPS endpoint present
    /// only when the posture enables it).
    pub fn effective_endpoints(&self) -> Vec<SpecEndpoint> {
        let endpoints = self
            .endpoints
            .clone()
            .unwrap_or_else(|| self.server.default_endpoints());
        if self.https.enabled {
            endpoints
        } else {
            endpoints
                .into_iter()
                .filter(|endpoint| endpoint.name != HTTPS_ENDPOINT)
                .collect()
        }
    }

    /// The primary container port (the first endpoint's — the
    /// health check's default target).
    pub fn primary_container_port(&self) -> u16 {
        self.effective_endpoints()
            .first()
            .map(|endpoint| endpoint.container_port)
            .unwrap_or(80)
    }

    /// The effective health check (the spec's own, or the default
    /// HTTP probe against the primary endpoint).
    pub fn effective_health_check(&self) -> HealthCheckSpec {
        self.health_check.clone().unwrap_or_else(|| HealthCheckSpec {
            kind: zim_engine_workloads_sdk::HealthCheckKind::Http {
                path: "/".to_owned(),
                expected_status: None,
            },
            port: Some(HTTP_ENDPOINT.to_owned()),
            timeout_ms: zim_engine_workloads_sdk::health::DEFAULT_PROBE_TIMEOUT.as_millis() as u64,
        })
    }

    /// The release retention (the rollback window's width).
    pub fn retained_releases(&self) -> usize {
        self.retained_releases
            .unwrap_or(DEFAULT_RETAINED_RELEASES)
            .max(1)
    }

    /// The server's environment (empty for every M19 class — the
    /// carriers need no bootstrap env the way the database
    /// images did; the spec's extra env lands with the engine's
    /// spec surface growing one).
    pub fn resolved_env(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// The resolved server argv: the server table's default
    /// command plus the spec's extra args.
    pub fn resolved_command(&self) -> Vec<String> {
        let mut argv = self.server.default_command();
        argv.extend(self.extra_args.iter().cloned());
        argv
    }

    /// Validate the spec's structural invariants.
    ///
    /// Strict where the engine must not launch garbage (a
    /// malformed image reference, a duplicate or empty endpoint
    /// name, a malformed health check, a retention of zero —
    /// normalized to one, so accepted); permissive where the
    /// server's own runtime surfaces the failure (extra args
    /// pass through verbatim).
    pub fn validate(&self) -> Result<(), SpecError> {
        let image = self.effective_image();
        if image.trim().is_empty() {
            return Err(SpecError::Invalid {
                field: "image".to_owned(),
                message: "the workload image must not be empty".to_owned(),
            });
        }
        if DockerImage::from_str(image).is_err() {
            return Err(SpecError::Invalid {
                field: "image".to_owned(),
                message: format!("{image:?} is not a valid image reference"),
            });
        }
        let endpoints = self.effective_endpoints();
        if endpoints.is_empty() {
            return Err(SpecError::Invalid {
                field: "endpoints".to_owned(),
                message: "the workload must expose at least one endpoint".to_owned(),
            });
        }
        let mut names = std::collections::BTreeSet::new();
        for endpoint in &endpoints {
            if endpoint.name.trim().is_empty() {
                return Err(SpecError::Invalid {
                    field: "endpoints".to_owned(),
                    message: "an endpoint has an empty name".to_owned(),
                });
            }
            if !names.insert(endpoint.name.clone()) {
                return Err(SpecError::Invalid {
                    field: "endpoints".to_owned(),
                    message: format!("the endpoint name {:?} is declared twice", endpoint.name),
                });
            }
            if endpoint.container_port == 0 {
                return Err(SpecError::Invalid {
                    field: "endpoints".to_owned(),
                    message: format!(
                        "the endpoint {:?} declares port 0",
                        endpoint.name
                    ),
                });
            }
        }
        if let Some(check) = &self.health_check {
            check.validate()?;
        }
        Ok(())
    }

    /// The server's container posture for the runtime-facing
    /// instance configuration (the per-instance user and
    /// rootfs overrides the M18 engine-side carries).
    pub fn container_posture(&self) -> ContainerPosture {
        ContainerPosture {
            user: self.server.container_user().map(str::to_owned),
            readonly_rootfs: !self.server.writable_rootfs(),
        }
    }
}

/// The default spec: a static nginx site with the default health
/// check and release retention.
pub fn default_spec() -> WebsiteSpec {
    WebsiteSpec {
        server: WebServer::Static,
        image: None,
        version: None,
        endpoints: None,
        health_check: None,
        https: HttpsSpec::default(),
        retained_releases: None,
        extra_args: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_spec_validates_and_resolves() {
        let spec = default_spec();
        spec.validate().unwrap();
        assert_eq!(spec.effective_image(), "nginx:1.27-alpine");
        assert_eq!(spec.primary_container_port(), 80);
        // HTTPS disabled: only the http endpoint.
        let endpoints = spec.effective_endpoints();
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].name, "http");
        // The default health check is an HTTP probe on http.
        let check = spec.effective_health_check();
        assert_eq!(check.port.as_deref(), Some("http"));
        assert!(matches!(
            check.kind,
            zim_engine_workloads_sdk::HealthCheckKind::Http { .. }
        ));
        assert!(spec.resolved_env().is_empty());
        assert_eq!(spec.resolved_command()[0], "nginx");
        assert_eq!(spec.retained_releases(), 3);
        // The static carrier's posture.
        let posture = spec.container_posture();
        assert_eq!(posture.user.as_deref(), Some("0:0"));
        assert!(!posture.readonly_rootfs);
    }

    #[test]
    fn the_https_posture_controls_the_https_endpoint() {
        let mut spec = default_spec();
        spec.https = HttpsSpec {
            enabled: true,
            certificate_ref: Some("secrets/cert-1".to_owned()),
        };
        let endpoints = spec.effective_endpoints();
        assert_eq!(endpoints.len(), 2);
        assert!(endpoints.iter().any(|e| e.name == "https"));
    }

    #[test]
    fn a_malformed_image_is_refused() {
        let mut spec = default_spec();
        spec.image = Some("nginx :1.27".to_owned());
        assert!(spec.validate().is_err());
        spec.image = Some(String::new());
        assert!(spec.validate().is_err());
        spec.image = None;
        spec.validate().unwrap();
    }

    #[test]
    fn endpoint_shapes_are_validated() {
        let mut spec = default_spec();
        spec.endpoints = Some(Vec::new());
        assert!(spec.validate().is_err());

        spec.endpoints = Some(vec![
            SpecEndpoint::new("http", 80),
            SpecEndpoint::new("http", 8080),
        ]);
        let err = spec.validate().unwrap_err();
        assert!(err.to_string().contains("twice"), "{err}");

        spec.endpoints = Some(vec![SpecEndpoint::new("", 80)]);
        assert!(spec.validate().is_err());

        spec.endpoints = Some(vec![SpecEndpoint::new("http", 0)]);
        assert!(spec.validate().is_err());

        spec.endpoints = Some(vec![SpecEndpoint::new("http", 80)]);
        spec.validate().unwrap();
    }

    #[test]
    fn a_malformed_health_check_is_refused() {
        let mut spec = default_spec();
        spec.health_check = Some(HealthCheckSpec {
            kind: zim_engine_workloads_sdk::HealthCheckKind::Http {
                path: "/a b".to_owned(),
                expected_status: None,
            },
            port: Some("http".to_owned()),
            timeout_ms: 0,
        });
        assert!(spec.validate().is_err());
    }

    #[test]
    fn the_node_server_resolves_its_shapes() {
        let spec = WebsiteSpec {
            server: WebServer::Node,
            image: None,
            version: None,
            endpoints: None,
            health_check: None,
            https: HttpsSpec::default(),
            retained_releases: None,
            extra_args: vec!["--inspect".to_owned()],
        };
        spec.validate().unwrap();
        assert_eq!(spec.effective_image(), "node:22-alpine");
        assert_eq!(spec.primary_container_port(), 3000);
        let command = spec.resolved_command();
        assert_eq!(command[0], "node");
        assert!(command.contains(&"--inspect".to_owned()));
        let posture = spec.container_posture();
        assert!(posture.user.is_none());
        assert!(posture.readonly_rootfs);
    }

    #[test]
    fn the_spec_round_trips_through_serde() {
        let spec = default_spec();
        let json = serde_json::to_value(&spec).unwrap();
        let back: WebsiteSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn a_minimal_json_spec_parses_with_server_defaults() {
        let spec: WebsiteSpec =
            serde_json::from_value(serde_json::json!({ "server": "python" })).unwrap();
        spec.validate().unwrap();
        assert_eq!(spec.effective_image(), "python:3.12-alpine");
        assert_eq!(spec.primary_container_port(), 8000);
        assert_eq!(spec.retained_releases(), 3);
    }

    #[test]
    fn an_unknown_server_is_unparsable() {
        let err = serde_json::from_value::<WebsiteSpec>(serde_json::json!({
            "server": "apache"
        }))
        .unwrap_err();
        assert!(err.to_string().contains("apache"), "{err}");
    }
}
