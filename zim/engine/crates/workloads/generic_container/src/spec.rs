//! The GenericContainer workload spec (M19).
//!
//! Per `docs/architecture/WORKLOAD_ABSTRACTION.md` §3, a
//! `Workload` row of kind `GenericContainer` carries this spec
//! as its `spec` JSON. The kind is the escape hatch: an
//! arbitrary image, an optional command, the environment, the
//! endpoints, a health check, and — the kind's own headline —
//! the **capability requirements the placement must satisfy**
//! (§4's "capability negotiation at placement time: whatever the
//! spec declares is what the runtime must provide"; see
//! [`crate::negotiate`]).
//!
//! The spec's *derived* requirements come from both halves: the
//! explicit [`required_capabilities`] list, and the spec's own
//! content (a health check requires `HealthCheckCapable`; a
//! non-empty endpoint list requires `NetworkCapable`).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use zim_engine_domain::DockerImage;
use zim_engine_workloads_sdk::errors::SpecError;
use zim_engine_workloads_sdk::{Capability, CapabilitySet, HealthCheckSpec};

/// One logical endpoint the container exposes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecEndpoint {
    /// The endpoint's logical name (`"http"`, `"metrics"`, ...).
    pub name: String,
    /// The container-side port.
    pub container_port: u16,
    /// The protocol (`"tcp"` or `"udp"`).
    #[serde(default = "default_protocol")]
    pub protocol: String,
}

/// The default endpoint protocol.
fn default_protocol() -> String {
    "tcp".to_owned()
}

impl SpecEndpoint {
    /// Construct one TCP endpoint declaration.
    pub fn new(name: &str, container_port: u16) -> Self {
        Self {
            name: name.to_owned(),
            container_port,
            protocol: default_protocol(),
        }
    }
}

/// The advisory resource limits (§3's `ResourceLimits`, the
/// spec-shaped half; the engine folds them into the `Instance`
/// row's own limits at placement, which is what the runtime
/// enforces).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ResourceLimitsSpec {
    /// The memory limit, in whole megabytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u32>,
    /// The CPU limit (1.0 = one full core).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<f32>,
}

/// The GenericContainer workload spec.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenericContainerSpec {
    /// The image to run.
    pub image: String,
    /// The command override (argv form; `None` keeps the image's
    /// own entrypoint/CMD).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// The environment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<(String, String)>,
    /// The logical endpoints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub endpoints: Vec<SpecEndpoint>,
    /// The health check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_check: Option<HealthCheckSpec>,
    /// The advisory resource limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_limits: Option<ResourceLimitsSpec>,
    /// The explicit capability requirements (the placement must
    /// provide these, plus whatever the spec's own content
    /// implies — see [`GenericContainerSpec::derived_requirements`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_capabilities: Vec<Capability>,
}

impl GenericContainerSpec {
    /// The effective command (the override or `None` for the
    /// image's own).
    pub fn effective_command(&self) -> Option<Vec<String>> {
        self.command.clone()
    }

    /// The spec's **derived** capability requirements: the
    /// explicit list plus the content-implied ones.
    ///
    /// - A health check requires `HealthCheckCapable`.
    /// - A non-empty endpoint list requires `NetworkCapable`.
    /// - A command override requires `ExecCapable`-class runtime
    ///   knowledge? No — the argv is the runtime's own start
    ///   surface, not an exec; it implies nothing here.
    ///
    /// This is §4's negotiation input: "whatever the spec
    /// declares is what the runtime must provide", with the
    /// spec's own content counted as a declaration.
    pub fn derived_requirements(&self) -> CapabilitySet {
        let mut required = CapabilitySet::EMPTY;
        for capability in &self.required_capabilities {
            required.insert(*capability);
        }
        if self.health_check.is_some() {
            required.insert(Capability::HealthCheckCapable);
        }
        if !self.endpoints.is_empty() {
            required.insert(Capability::NetworkCapable);
        }
        required
    }

    /// Validate the spec's structural invariants.
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.image.trim().is_empty() {
            return Err(SpecError::Invalid {
                field: "image".to_owned(),
                message: "the workload image must not be empty".to_owned(),
            });
        }
        if DockerImage::from_str(&self.image).is_err() {
            return Err(SpecError::Invalid {
                field: "image".to_owned(),
                message: format!("{:?} is not a valid image reference", self.image),
            });
        }
        if let Some(command) = &self.command {
            if command.is_empty() {
                return Err(SpecError::Invalid {
                    field: "command".to_owned(),
                    message: "the command override must not be empty (omit it to keep \
                         the image's own)"
                        .to_owned(),
                });
            }
            if command.iter().any(|arg| arg.is_empty()) {
                return Err(SpecError::Invalid {
                    field: "command".to_owned(),
                    message: "the command carries an empty argument".to_owned(),
                });
            }
        }
        let mut names = std::collections::BTreeSet::new();
        for endpoint in &self.endpoints {
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
                    message: format!("the endpoint {:?} declares port 0", endpoint.name),
                });
            }
            if endpoint.protocol != "tcp" && endpoint.protocol != "udp" {
                return Err(SpecError::Invalid {
                    field: "endpoints".to_owned(),
                    message: format!(
                        "the endpoint {:?} declares an unknown protocol {:?} (tcp or udp)",
                        endpoint.name, endpoint.protocol
                    ),
                });
            }
        }
        if let Some(check) = &self.health_check {
            check.validate()?;
        }
        if let Some(limits) = &self.resource_limits {
            if limits.memory_mb.is_some_and(|mb| mb == 0) {
                return Err(SpecError::Invalid {
                    field: "resource_limits.memory_mb".to_owned(),
                    message: "the memory limit must be positive".to_owned(),
                });
            }
            if limits.cpu.is_some_and(|cpu| cpu <= 0.0) {
                return Err(SpecError::Invalid {
                    field: "resource_limits.cpu".to_owned(),
                    message: "the CPU limit must be positive".to_owned(),
                });
            }
        }
        Ok(())
    }
}

/// The default spec: a busybox long-sleep container with no
/// requirements — the minimal generic workload (a placement that
/// provides nothing at all can take it).
pub fn default_spec() -> GenericContainerSpec {
    GenericContainerSpec {
        image: "busybox:1.36".to_owned(),
        command: Some(vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "while true; do sleep 5; done".to_owned(),
        ]),
        env: Vec::new(),
        endpoints: Vec::new(),
        health_check: None,
        resource_limits: None,
        required_capabilities: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_spec_validates_and_requires_nothing() {
        let spec = default_spec();
        spec.validate().unwrap();
        assert_eq!(spec.derived_requirements(), CapabilitySet::EMPTY);
    }

    #[test]
    fn the_content_implies_its_requirements() {
        let mut spec = default_spec();
        spec.health_check = Some(HealthCheckSpec::tcp("http"));
        let derived = spec.derived_requirements();
        assert!(derived.contains(Capability::HealthCheckCapable));
        assert!(!derived.contains(Capability::NetworkCapable));

        spec.endpoints = vec![SpecEndpoint::new("http", 8080)];
        let derived = spec.derived_requirements();
        assert!(derived.contains(Capability::NetworkCapable));

        // The explicit list rides along.
        spec.required_capabilities = vec![Capability::ExecCapable];
        assert!(spec.derived_requirements().contains(Capability::ExecCapable));
    }

    #[test]
    fn a_malformed_image_is_refused() {
        let mut spec = default_spec();
        spec.image = "busy box".to_owned();
        assert!(spec.validate().is_err());
        spec.image = String::new();
        assert!(spec.validate().is_err());
    }

    #[test]
    fn an_empty_command_override_is_refused() {
        let mut spec = default_spec();
        spec.command = Some(Vec::new());
        assert!(spec.validate().is_err());
        spec.command = Some(vec!["run".to_owned(), String::new()]);
        assert!(spec.validate().is_err());
        spec.command = None;
        spec.validate().unwrap();
    }

    #[test]
    fn the_endpoint_shapes_are_validated() {
        let mut spec = default_spec();
        spec.endpoints = vec![SpecEndpoint::new("", 80)];
        assert!(spec.validate().is_err());
        spec.endpoints = vec![
            SpecEndpoint::new("http", 80),
            SpecEndpoint::new("http", 81),
        ];
        assert!(spec.validate().is_err());
        spec.endpoints = vec![SpecEndpoint::new("http", 0)];
        assert!(spec.validate().is_err());
        spec.endpoints = vec![SpecEndpoint {
            name: "dns".to_owned(),
            container_port: 53,
            protocol: "sctp".to_owned(),
        }];
        assert!(spec.validate().is_err());
        spec.endpoints = vec![SpecEndpoint {
            name: "dns".to_owned(),
            container_port: 53,
            protocol: "udp".to_owned(),
        }];
        spec.validate().unwrap();
    }

    #[test]
    fn the_resource_limits_are_validated() {
        let mut spec = default_spec();
        spec.resource_limits = Some(ResourceLimitsSpec {
            memory_mb: Some(0),
            cpu: None,
        });
        assert!(spec.validate().is_err());
        spec.resource_limits = Some(ResourceLimitsSpec {
            memory_mb: Some(512),
            cpu: Some(0.0),
        });
        assert!(spec.validate().is_err());
        spec.resource_limits = Some(ResourceLimitsSpec {
            memory_mb: Some(512),
            cpu: Some(1.5),
        });
        spec.validate().unwrap();
    }

    #[test]
    fn the_spec_round_trips_through_serde() {
        let spec = default_spec();
        let json = serde_json::to_value(&spec).unwrap();
        let back: GenericContainerSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, spec);

        let json = serde_json::json!({
            "image": "nginx:1.27-alpine",
            "endpoints": [{ "name": "http", "container_port": 80 }],
            "health_check": { "kind": { "type": "tcp" }, "port": "http", "timeout_ms": 2000 },
            "required_capabilities": ["MetricsCapable"]
        });
        let spec: GenericContainerSpec = serde_json::from_value(json).unwrap();
        spec.validate().unwrap();
        let derived = spec.derived_requirements();
        for capability in [
            Capability::MetricsCapable,
            Capability::HealthCheckCapable,
            Capability::NetworkCapable,
        ] {
            assert!(derived.contains(capability));
        }
    }

    #[test]
    fn an_unknown_capability_is_unparsable() {
        let json = serde_json::json!({
            "image": "busybox:1.36",
            "required_capabilities": ["TelepathyCapable"]
        });
        assert!(serde_json::from_value::<GenericContainerSpec>(json).is_err());
    }
}
