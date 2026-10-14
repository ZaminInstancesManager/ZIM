//! The supervision vocabulary: runtime kinds, desired/observed
//! lifecycle states, the runtime-facing instance configuration, and
//! the runtime error vocabulary.

use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use zim_engine_domain::instance::ResourceLimits;
use zim_engine_domain::InstanceId;

/// Which class of runtime backs an instance (invariant 8: the
/// engine's security policy treats them differently — the host
/// runtime is opt-in per instance and requires admin consent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeKind {
    /// An isolated container runtime (Docker).
    Container,
    /// A native process on the host, without isolation (opt-in,
    /// admin-consent-gated; not shipped in M6).
    Host,
}

/// The desired lifecycle state for an instance.
///
/// Set through the power queue (never directly) so exactly one
/// desired state exists at a time; the power worker resolves
/// transient entries (`Restarted`, `Killed`) into their steady
/// forms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesiredState {
    /// The instance should be running.
    Started,
    /// The instance should be stopped (graceful stop completed or
    /// requested).
    Stopped,
    /// A restart was requested (transient; resolves to `Started`).
    Restarted,
    /// A force-kill was requested (transient; resolves to
    /// `Stopped`).
    Killed,
    /// The instance is suspended: stopped, and lifecycle-start is
    /// rejected until an admin lifts the suspension.
    Suspended,
}

impl DesiredState {
    /// Whether this desired state asks for the instance to end up
    /// running (`Started` and the transient `Restarted` both do).
    pub fn wants_running(&self) -> bool {
        matches!(self, Self::Started | Self::Restarted)
    }

    /// The steady-state form (transient entries collapse into their
    /// outcomes once the power worker finishes with them).
    pub fn steady(&self) -> Self {
        match self {
            Self::Restarted => Self::Started,
            Self::Killed => Self::Stopped,
            other => *other,
        }
    }
}

/// The observed lifecycle state, as reported by the runtime.
///
/// The first four variants are the Wings base lifecycle
/// (`ProcessOfflineState`/`ProcessStartingState`/`ProcessRunningState`/
/// `ProcessStoppingState`); `Unknown` is the runtime-unreachable form
/// required by the failure matrix in
/// `docs/architecture/ZIM_ARCHITECTURE.md` §5 (a disconnected node's
/// instances are `Unknown`, never `Offline`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservedState {
    /// Not running. `exit_code`/`oom_killed` come from the runtime's
    /// last-known exit state when one exists.
    Offline {
        /// The process exit code, when the container has exited.
        exit_code: Option<i32>,
        /// Whether the exit was caused by the OOM killer.
        oom_killed: bool,
    },
    /// A start is in flight (set by the power worker; a container
    /// runtime never reports "starting" — the transition is
    /// engine-side).
    Starting,
    /// Running and observed alive.
    Running {
        /// When the runtime reported the process start.
        started_at: DateTime<Utc>,
        /// The host-side pid, when the runtime exposes one.
        pid: Option<u32>,
    },
    /// A stop is in flight.
    Stopping {
        /// When the stop began (deadline anchors here).
        since: DateTime<Utc>,
    },
    /// The runtime could not be reached. Not the same as offline.
    Unknown,
}

impl ObservedState {
    /// The `InstanceStatus` form for persistence and the API.
    ///
    /// `CrashLoop` is *not* produced here: it is a supervision-level
    /// overlay (crash history + policy) layered on top of an
    /// `Offline` observation; the supervisor applies it when
    /// persisting and answering state queries.
    pub fn status(&self) -> zim_engine_domain::InstanceStatus {
        use zim_engine_domain::InstanceStatus as S;
        match self {
            Self::Offline { .. } => S::Offline,
            Self::Starting => S::Starting,
            Self::Running { .. } => S::Running,
            Self::Stopping { .. } => S::Stopping,
            Self::Unknown => S::Unknown,
        }
    }

    /// Whether the observed state is the running form.
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }
}

/// A host→container port mapping for an instance's allocations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortBinding {
    /// The host-side IP to publish on (the allocation's IP).
    pub host_ip: IpAddr,
    /// The host-side port (the allocation's port).
    pub host_port: u16,
    /// The container-side port the workload listens on. Pterodactyl
    /// maps host and container ports 1:1; ZIM keeps the convention.
    pub container_port: u16,
    /// L4 protocol for the mapping.
    pub protocol: PortProtocol,
}

/// The L4 protocol of a [`PortBinding`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PortProtocol {
    /// TCP (the default; what game-server allocations use).
    #[default]
    Tcp,
    /// UDP (some game eggs request it).
    Udp,
}

impl PortProtocol {
    /// The Docker `port/<proto>` suffix.
    pub fn docker_suffix(&self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

/// The runtime-facing configuration for one instance: everything the
/// container runtime needs to (re)create and start the instance's
/// container.
///
/// Built by [`build_instance_config`](super::instance::build_instance_config)
/// from the persisted `Instance` row, its primary allocation, and the
/// engine's data layout. The runtime treats it as opaque; the
/// supervision layer refreshes it before every start so a recreated
/// container always matches the persisted row (Wings's
/// `OnBeforeStart` recreate-to-sync behavior).
#[derive(Debug, Clone)]
pub struct InstanceConfig {
    /// The instance this configuration belongs to.
    pub instance_id: InstanceId,
    /// The container name (the instance's legacy UUID, Wings's
    /// convention — stable across restarts, unique per instance).
    pub container_name: String,
    /// The container hostname (the legacy UUID as well; Wings sets
    /// `Hostname: uuid`).
    pub hostname: String,
    /// The image reference to run.
    pub image: String,
    /// Optional entrypoint override (egg-defined; empty in M6 — see
    /// [`build_instance_config`](super::instance::build_instance_config)).
    pub entrypoint: Option<Vec<String>>,
    /// The resolved startup command (argv form).
    pub command: Vec<String>,
    /// The container environment (`KEY=value` pairs).
    pub env: Vec<(String, String)>,
    /// Resource limits (memory/swap/cpu/blkio/pinning/oom policy).
    pub limits: ResourceLimits,
    /// Host→container port mappings for the instance's allocations.
    pub port_bindings: Vec<PortBinding>,
    /// The host directory bind-mounted as the instance's data
    /// volume (Wings mounts at `/home/container`).
    pub data_dir: PathBuf,
    /// The path the data dir mounts at inside the container.
    pub container_data_dir: PathBuf,
    /// Container labels (engine bookkeeping + Wings-parity marks).
    pub labels: Vec<(String, String)>,
    /// Whether crash detection is enabled for this instance.
    pub crash_detection_enabled: bool,
}

/// A one-shot command execution request (the `exec` capability;
/// used by install jobs and engine tooling).
#[derive(Debug, Clone, Default)]
pub struct ExecCommand {
    /// The command to run (argv form).
    pub cmd: Vec<String>,
    /// Extra environment entries for the exec process.
    pub env: Vec<(String, String)>,
    /// Override the exec user (`uid:gid` form); `None` keeps the
    /// container's user.
    pub user: Option<String>,
    /// Working directory override; `None` keeps the container's.
    pub working_dir: Option<String>,
    /// How long the exec may run before it is treated as failed
    /// (`None` = the supervision default).
    pub timeout: Option<Duration>,
}

/// The collected result of an [`ExecCommand`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecOutput {
    /// The exec's exit code (`None` when it could not be observed).
    pub exit_code: Option<i64>,
    /// Everything the exec wrote to stdout.
    pub stdout: Vec<u8>,
    /// Everything the exec wrote to stderr.
    pub stderr: Vec<u8>,
}

/// A one-shot resource-usage sample (the `stats` capability; the
/// streaming metrics surface arrives with M7).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResourceUsage {
    /// Total CPU time consumed by the container's processes
    /// (nanoseconds), when reported.
    pub cpu_usage_total_ns: Option<u64>,
    /// The host's total CPU time over the same measurement window
    /// (nanoseconds), for utilization math.
    pub system_cpu_total_ns: Option<u64>,
    /// Online CPUs visible to the container, when reported.
    pub online_cpus: Option<u64>,
    /// Current memory usage in bytes (RSS + cache accounting
    /// per the runtime), when reported.
    pub memory_usage_bytes: Option<u64>,
    /// The memory limit in force (matches the instance limits when
    /// one is set).
    pub memory_limit_bytes: Option<u64>,
    /// Current pid count inside the container, when reported.
    pub pids_current: Option<u64>,
}

/// A live console attachment (M7.1): the decoded output-line stream
/// of one run of the instance's target.
///
/// [`Runtime::attach_console`](super::runtime::Runtime::attach_console)
/// establishes the attachment (the Docker adapter's bollard attach
/// pump) and returns this handle; the supervision layer owns the
/// consuming task that publishes each line onto the streaming hub as
/// an `instance.console_output` event (RUST_ENGINE.md §6). Lines are
/// decoded lossily — invalid UTF-8 becomes replacement characters —
/// and keep their newline as received from the target's attach
/// stream (a TTY's `\r\n` included); a target that dies mid-line has
/// its unterminated tail flushed without one.
///
/// The channel is bounded: the runtime's pump task applies
/// backpressure to the target's *output* only when the supervision
/// consumer stalls — it never blocks the runtime's lifecycle calls
/// (the pump is a standalone task, and `Runtime::send_input` writes
/// through the attachment's stdin side independently).
#[derive(Debug)]
pub struct ConsoleAttachment {
    /// Decoded console output lines, oldest first. The stream ends
    /// (returns `None`) when the target exits and the pump has
    /// flushed its partial-line tail — or when the consumer drops
    /// the receiver, which detaches the stdin side too.
    pub lines: tokio::sync::mpsc::Receiver<String>,
}

/// The error vocabulary of the runtime abstraction.
///
/// The supervision layer (and the API boundary above it) maps these
/// onto lifecycle decisions and structured HTTP errors; the
/// DockerRuntime maps `bollard` failures into them.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeError {
    /// The runtime backend is unreachable or unusable (Docker daemon
    /// down, no Docker installed). Observed state becomes `Unknown`,
    /// never `Offline`.
    #[error("runtime unavailable: {0}")]
    Unavailable(String),
    /// The target (container/image) does not exist. Lifecycle
    /// operations treat this as already-satisfied rather than an
    /// error where Wings does (404-tolerant stop/kill/remove).
    #[error("runtime reports target not found: {0}")]
    NotFound(String),
    /// An operation exceeded its deadline.
    #[error("runtime operation timed out: {0}")]
    Timeout(String),
    /// The configuration is not runnable as given.
    #[error("invalid instance configuration: {0}")]
    InvalidConfig(String),
    /// The operation requires the target to be running, and it is
    /// not — console input with no live attachment is the M7.1
    /// example. The API maps this onto its state-conflict vocabulary
    /// (`409`), not an internal error: the caller asked for something
    /// the instance's current lifecycle state cannot satisfy, and a
    /// retry after a start is meaningful.
    #[error("runtime target is not running: {0}")]
    NotRunning(String),
    /// The runtime does not implement this capability yet. Carries
    /// the milestone that will deliver it. After M7.1 no in-tree
    /// runtime answers this for the console surface; it remains the
    /// vocabulary for future capability gaps.
    #[error("runtime capability not implemented: {feature} (arrives with {milestone})")]
    Unsupported {
        /// Which capability was requested.
        feature: String,
        /// The roadmap milestone that will implement it.
        milestone: String,
    },
    /// Any other backend failure.
    #[error("runtime backend error: {0}")]
    Backend(String),
}

impl RuntimeError {
    /// Convenience constructor for the unavailable form.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable(reason.into())
    }

    /// Whether this is the not-found form (callers decide which
    /// operations tolerate it).
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound(_))
    }

    /// Whether this is the unavailable form (observed state must
    /// become `Unknown`).
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

#[cfg(test)]
mod tests {
    //! Vocabulary-level checks: status mapping and desired-state
    //! resolution (the behavioral invariants are exercised by the
    //! supervision integration tests with the mock runtime).

    use super::*;
    use zim_engine_domain::InstanceStatus;

    #[test]
    fn observed_state_maps_to_instance_status() {
        assert_eq!(
            ObservedState::Offline {
                exit_code: Some(1),
                oom_killed: false
            }
            .status(),
            InstanceStatus::Offline
        );
        assert_eq!(ObservedState::Starting.status(), InstanceStatus::Starting);
        assert_eq!(
            ObservedState::Running {
                started_at: Utc::now(),
                pid: None
            }
            .status(),
            InstanceStatus::Running
        );
        assert_eq!(
            ObservedState::Stopping { since: Utc::now() }.status(),
            InstanceStatus::Stopping
        );
        assert_eq!(ObservedState::Unknown.status(), InstanceStatus::Unknown);
    }

    #[test]
    fn desired_state_resolution() {
        assert!(DesiredState::Started.wants_running());
        assert!(DesiredState::Restarted.wants_running());
        assert!(!DesiredState::Stopped.wants_running());
        assert!(!DesiredState::Killed.wants_running());
        assert!(!DesiredState::Suspended.wants_running());

        assert_eq!(DesiredState::Restarted.steady(), DesiredState::Started);
        assert_eq!(DesiredState::Killed.steady(), DesiredState::Stopped);
        assert_eq!(DesiredState::Suspended.steady(), DesiredState::Suspended);
    }

    #[test]
    fn runtime_error_predicates() {
        assert!(RuntimeError::NotFound("gone".into()).is_not_found());
        assert!(RuntimeError::unavailable("daemon down").is_unavailable());
        assert!(!RuntimeError::Backend("boom".into()).is_unavailable());
        // The not-running form is distinct from unavailable and
        // backend failures (the API's 409 mapping keys on it).
        let err = RuntimeError::NotRunning("no attachment".to_owned());
        assert!(!err.is_unavailable());
        assert!(!err.is_not_found());
        assert!(err.to_string().contains("not running"));
        // The error Display includes the milestone for a capability
        // gap's structured surfacing.
        let err = RuntimeError::Unsupported {
            feature: "future capability".to_owned(),
            milestone: "M16 (workloads)".to_owned(),
        };
        assert!(err.to_string().contains("M16"));
    }
}
