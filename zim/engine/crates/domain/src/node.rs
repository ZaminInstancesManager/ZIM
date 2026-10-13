//! Node and Allocation entities.
//!
//! # Source of truth
//!
//! `Node`, `NodeCapabilities`, `NodeState`, `NodeDeployment`, and
//! `Allocation` mirror `docs/architecture/RUST_ENGINE.md` §3. The
//! `NodeDeployment` variants carry the Local/Remote distinction from
//! `docs/architecture/ZIM_ARCHITECTURE.md` §5 (remote-node contract),
//! with the host-runtime caveat (LocalWindows/LocalMacOS are host
//! runtimes, documented as constrained in
//! `docs/architecture/PLATFORM_SUPPORT.md`).
//!
//! `NodeCapabilities` models which optional capability *families*
//! (per `docs/architecture/WORKLOAD_ABSTRACTION.md` §4) the node's
//! container runtime supports; the full typed capability traits live
//! in `zim_engine_application::supervision::capabilities` from
//! milestone M6.

use crate::id::{InstanceId, LocationId, NodeId};
use crate::value::Scheme;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use thiserror::Error;

/// A node: an execution host for instances (local or remote).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// Strongly-typed node identifier.
    pub id: NodeId,
    /// Display name (non-empty).
    pub name: String,
    /// Location grouping (datacenter / region).
    pub location_id: LocationId,
    /// Fully-qualified domain name for the remote-node contract
    /// (loopback-resolvable name for the local node).
    pub fqdn: String,
    /// Scheme for the remote-node contract endpoint.
    pub scheme: Scheme,
    /// Port advertised to clients (SFTP/legacy compat surface).
    pub public_port: u16,
    /// Port the engine's node contract listens on.
    pub listen_port: u16,
    /// Which optional capability families this node supports.
    pub capabilities: NodeCapabilities,
    /// Deployment shape (local vs remote Linux).
    pub deployment: NodeDeployment,
    /// Last heartbeat received (None = never seen).
    pub last_heartbeat: Option<DateTime<Utc>>,
    /// Last observed connectivity state.
    ///
    /// The engine treats a disconnected node's instances as
    /// `InstanceStatus::Unknown` (not Offline) per the failure
    /// matrix in `docs/architecture/ZIM_ARCHITECTURE.md` §5.
    pub last_seen_state: NodeState,
}

impl Node {
    /// Validate the invariants the constructor cannot enforce.
    pub fn validate(&self) -> Result<(), NodeError> {
        if self.name.trim().is_empty() {
            return Err(NodeError::EmptyName);
        }
        if self.fqdn.trim().is_empty() {
            return Err(NodeError::EmptyFqdn);
        }
        if self.listen_port == 0 {
            return Err(NodeError::ZeroListenPort);
        }
        Ok(())
    }
}

/// Domain-level validation errors for Node.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NodeError {
    /// The node name is empty or whitespace-only.
    #[error("node name must not be empty")]
    EmptyName,
    /// The FQDN is empty.
    #[error("node fqdn must not be empty")]
    EmptyFqdn,
    /// The listen port is 0 (reserved).
    #[error("listen port must not be 0")]
    ZeroListenPort,
}

/// Where the engine and the container runtime live, relative to each
/// other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NodeDeployment {
    /// Engine and runtime on the same Linux machine (the default
    /// single-machine ZIM deployment).
    #[default]
    LocalLinux,
    /// Remote Linux node managed over the authenticated node
    /// contract.
    RemoteLinux,
    // LocalWindows / LocalMacOS are *host* runtimes (ZIM extension),
    // not container runtimes; documented as constrained in
    // PLATFORM_SUPPORT.md and intentionally not modeled here yet.
}

/// Which optional capability families a node's runtime supports.
///
/// Mirrors the capability trait families in
/// `docs/architecture/WORKLOAD_ABSTRACTION.md` §4. Placement checks
/// (E1/E2 in TEST_STRATEGY.md) compare a workload's required
/// capabilities against the *runtime's* declared capabilities; this
/// node-level set is the coarse pre-filter used by the placement
/// planner before querying the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeCapabilities {
    /// The node runs a container runtime (Docker). Always true for
    /// container deployments; false only for a future host-runtime
    /// node.
    pub container_runtime: bool,
    /// Console streams are available (ConsoleCapable).
    pub console: bool,
    /// Readiness probes are available (ReadyProbeCapable).
    pub ready_probe: bool,
    /// Health checks are available (HealthCheckCapable).
    pub health_check: bool,
    /// Filesystem operations are available (FilesystemCapable).
    pub filesystem: bool,
    /// Multi-endpoint networking is available (NetworkCapable).
    pub network: bool,
    /// Snapshot-style backups are available (SnapshotCapable).
    pub snapshot: bool,
}

impl Default for NodeCapabilities {
    /// The baseline node: a container runtime with no optional
    /// capability families declared. The DockerRuntime adapter will
    /// declare its concrete capability set in milestone M6.
    fn default() -> Self {
        Self {
            container_runtime: true,
            console: false,
            ready_probe: false,
            health_check: false,
            filesystem: false,
            network: false,
            snapshot: false,
        }
    }
}

/// Observed connectivity state of a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    /// Heartbeats arriving within the timeout window.
    Online,
    /// No heartbeat within the timeout window (or explicit shutdown).
    Offline,
    /// Never seen / state not yet established.
    #[default]
    Unknown,
}

/// An IP + port assignment on a node, assignable to an instance.
///
/// `AllocationId` is `u64` (Pterodactyl schema compat per AD-12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Allocation {
    /// Allocation identifier (u64, legacy compat).
    pub id: crate::id::AllocationId,
    /// The node whose address space this allocation belongs to.
    pub node_id: NodeId,
    /// The assigned IP address.
    pub ip: IpAddr,
    /// The assigned port (1–65535; 0 is invalid).
    pub port: u16,
    /// The instance this allocation is assigned to (None = free).
    pub assigned_to: Option<InstanceId>,
    /// Operator note (e.g. "voice chat").
    pub note: Option<String>,
}

impl Allocation {
    /// Validate the invariants the constructor cannot enforce.
    pub fn validate(&self) -> Result<(), AllocationError> {
        if self.port == 0 {
            return Err(AllocationError::ZeroPort);
        }
        if matches!(self.ip, IpAddr::V4(ip4) if ip4.is_unspecified()) {
            return Err(AllocationError::UnspecifiedIp);
        }
        if matches!(self.ip, IpAddr::V6(ip6) if ip6.is_unspecified()) {
            return Err(AllocationError::UnspecifiedIp);
        }
        Ok(())
    }
}

/// Domain-level validation errors for Allocation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AllocationError {
    /// The port is 0 (reserved).
    #[error("allocation port must not be 0")]
    ZeroPort,
    /// The IP is unspecified (0.0.0.0 / ::), which cannot be
    /// *assigned* (it is a bind wildcard, not an address).
    #[error("allocation ip must not be unspecified")]
    UnspecifiedIp,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{AllocationId, LocationId, NodeId};

    fn sample_node() -> Node {
        Node {
            id: NodeId::new(),
            name: "node-hetzner-1".to_owned(),
            location_id: LocationId::new(),
            fqdn: "node1.example.org".to_owned(),
            scheme: Scheme::Https,
            public_port: 2022,
            listen_port: 8443,
            capabilities: NodeCapabilities {
                container_runtime: true,
                console: true,
                ready_probe: false,
                health_check: true,
                filesystem: true,
                network: false,
                snapshot: true,
            },
            deployment: NodeDeployment::LocalLinux,
            last_heartbeat: Some(Utc::now()),
            last_seen_state: NodeState::Online,
        }
    }

    #[test]
    fn node_round_trips_through_serde() {
        let node = sample_node();
        let json = serde_json::to_string(&node).unwrap();
        let back: Node = serde_json::from_str(&json).unwrap();
        assert_eq!(node, back);
    }

    #[test]
    fn node_validate_accepts_sample() {
        assert!(sample_node().validate().is_ok());
    }

    #[test]
    fn node_validate_rejects_empty_name() {
        let mut node = sample_node();
        node.name = String::new();
        assert!(matches!(node.validate(), Err(NodeError::EmptyName)));
    }

    #[test]
    fn node_validate_rejects_empty_fqdn() {
        let mut node = sample_node();
        node.fqdn = " ".to_owned();
        assert!(matches!(node.validate(), Err(NodeError::EmptyFqdn)));
    }

    #[test]
    fn node_validate_rejects_zero_listen_port() {
        let mut node = sample_node();
        node.listen_port = 0;
        assert!(matches!(node.validate(), Err(NodeError::ZeroListenPort)));
    }

    #[test]
    fn node_deployment_defaults_to_local_linux() {
        assert_eq!(NodeDeployment::default(), NodeDeployment::LocalLinux);
    }

    #[test]
    fn node_state_defaults_to_unknown() {
        assert_eq!(NodeState::default(), NodeState::Unknown);
    }

    #[test]
    fn capabilities_default_is_minimal() {
        // Default: container runtime on, optional families off. The
        // DockerRuntime will declare console+filesystem+snapshot as
        // supported in M6.
        let caps = NodeCapabilities::default();
        assert!(caps.container_runtime);
        assert!(!caps.console);
        assert!(!caps.snapshot);
    }

    fn sample_allocation() -> Allocation {
        Allocation {
            id: AllocationId::new(42),
            node_id: NodeId::new(),
            ip: "203.0.113.10".parse().unwrap(),
            port: 25565,
            assigned_to: None,
            note: Some("minecraft primary".to_owned()),
        }
    }

    #[test]
    fn allocation_round_trips_through_serde() {
        let alloc = sample_allocation();
        let json = serde_json::to_string(&alloc).unwrap();
        let back: Allocation = serde_json::from_str(&json).unwrap();
        assert_eq!(alloc, back);
    }

    #[test]
    fn allocation_ipv6_round_trips() {
        let mut alloc = sample_allocation();
        alloc.ip = "2001:db8::1".parse().unwrap();
        let json = serde_json::to_string(&alloc).unwrap();
        let back: Allocation = serde_json::from_str(&json).unwrap();
        assert_eq!(alloc, back);
        assert_eq!(back.ip, "2001:db8::1".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn allocation_validate_rejects_zero_port() {
        let mut alloc = sample_allocation();
        alloc.port = 0;
        assert!(matches!(alloc.validate(), Err(AllocationError::ZeroPort)));
    }

    #[test]
    fn allocation_validate_rejects_unspecified_ip() {
        let mut alloc = sample_allocation();
        alloc.ip = "0.0.0.0".parse().unwrap();
        assert!(matches!(
            alloc.validate(),
            Err(AllocationError::UnspecifiedIp)
        ));
    }
}
