//! The event vocabulary and the §11.1 wire envelope.
//!
//! `EventKind` is the `"type"` string of ZIM_API_SPEC.md §11.1 plus
//! the two meta-events (`stream.dropped`, `stream.gap`) the engine
//! inserts on its own behalf. `StreamEvent` is the in-engine form;
//! its `Serialize` impl produces the spec's envelope object
//! (`{type, generation, ts, instance_id, data}`) verbatim, so the
//! WebSocket writer serializes events directly.

use chrono::{DateTime, Utc};
use serde::Serialize;
use zim_engine_domain::{InstanceId, InstanceStatus, JobId, NodeId};

/// The WebSocket sub-protocol every `zim-events.v1` client speaks
/// (ZIM_API_SPEC.md §11: the only one currently supported).
pub const WIRE_SUBPROTOCOL: &str = "zim-events.v1";

/// The `"type"` discriminant of an event (ZIM_API_SPEC.md §11.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventKind {
    /// `instance.state_change` — an observed lifecycle transition.
    InstanceStateChange,
    /// `instance.console_output` — one console line from the
    /// instance's stdout/stderr attach stream.
    InstanceConsoleOutput,
    /// `instance.resource_usage` — a periodic resource-usage sample.
    InstanceResourceUsage,
    /// `instance.install_progress` — egg-install job progress.
    InstanceInstallProgress,
    /// `job.progress` — a durable job's progress update.
    JobProgress,
    /// `job.completed` — a durable job reached a terminal state.
    JobCompleted,
    /// `node.state_change` — a node's observed connectivity state
    /// changed (engine-global; no `instance_id`).
    NodeStateChange,
    /// `audit.record` — an audit-log append (engine-global unless
    /// scoped to an instance).
    AuditRecord,
    /// `stream.dropped` — meta: this subscriber's queue overflowed
    /// and oldest events were dropped (ZIM_API_SPEC.md §11.2).
    StreamDropped,
    /// `stream.gap` — meta: replay could not cover a generation
    /// range; the subscriber should re-fetch state via the HTTP API
    /// (ZIM_API_SPEC.md §11.3).
    StreamGap,
}

impl EventKind {
    /// The wire `"type"` string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InstanceStateChange => "instance.state_change",
            Self::InstanceConsoleOutput => "instance.console_output",
            Self::InstanceResourceUsage => "instance.resource_usage",
            Self::InstanceInstallProgress => "instance.install_progress",
            Self::JobProgress => "job.progress",
            Self::JobCompleted => "job.completed",
            Self::NodeStateChange => "node.state_change",
            Self::AuditRecord => "audit.record",
            Self::StreamDropped => "stream.dropped",
            Self::StreamGap => "stream.gap",
        }
    }

    /// Every kind (the default filter when a client asks for
    /// nothing specific).
    pub const ALL: [EventKind; 10] = [
        Self::InstanceStateChange,
        Self::InstanceConsoleOutput,
        Self::InstanceResourceUsage,
        Self::InstanceInstallProgress,
        Self::JobProgress,
        Self::JobCompleted,
        Self::NodeStateChange,
        Self::AuditRecord,
        Self::StreamDropped,
        Self::StreamGap,
    ];

    /// Whether this is an engine-inserted meta-event rather than a
    /// workload-produced event. Meta-events bypass the retention
    /// log (they describe the stream itself).
    pub const fn is_meta(self) -> bool {
        matches!(self, Self::StreamDropped | Self::StreamGap)
    }

    /// Whether events of this kind are subject to the console
    /// retention bounds (bounded by events and bytes) rather than
    /// the keep-all durable log (ZIM_API_SPEC.md §11.3).
    pub const fn is_console(self) -> bool {
        matches!(self, Self::InstanceConsoleOutput)
    }
}

/// The event-specific `data` payload (ZIM_API_SPEC.md §11.1's `data`
/// object; the shape of each variant is the event's contract).
///
/// Serialization is one-directional (the engine only *writes*
/// events); clients that need to re-ingest engine envelopes do so
/// as opaque JSON, so no `Deserialize` is provided — an untagged
/// round-trip could not distinguish overlapping payload shapes
/// (e.g. install vs. job progress).
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum EventData {
    /// `instance.state_change` — `from`/`to` in the §11.1 status
    /// vocabulary (snake_case `InstanceStatus` names).
    StateChange {
        /// The status observed before the transition.
        from: InstanceStatus,
        /// The status observed after the transition.
        to: InstanceStatus,
    },
    /// `instance.console_output` — one line of console output.
    ConsoleOutput {
        /// The line (UTF-8, newline-terminated as received from the
        /// attach stream).
        line: String,
    },
    /// `instance.resource_usage` — a point-in-time usage sample.
    ResourceUsage(ResourceUsageData),
    /// `instance.install_progress` — the install job's stage.
    InstallProgress {
        /// Named pipeline stage (e.g. `pulling_image`).
        stage: String,
        /// 0.0–1.0 completion of the stage.
        progress: f32,
    },
    /// `job.progress` — a durable job's stage.
    JobProgress {
        /// The job.
        job_id: JobId,
        /// Named job stage.
        stage: String,
        /// 0.0–1.0 completion of the stage.
        progress: f32,
    },
    /// `job.completed` — a job's terminal state.
    JobCompleted {
        /// The job.
        job_id: JobId,
        /// Whether the job succeeded.
        success: bool,
    },
    /// `node.state_change` — a node's connectivity state.
    NodeStateChange {
        /// The node.
        node_id: NodeId,
        /// The node's new state (snake_case `NodeState` name).
        state: String,
    },
    /// `audit.record` — an audit-log append.
    AuditRecord {
        /// The acting principal's identifier (user id or `system`).
        actor: String,
        /// The audited action (e.g. `instance.power.start`).
        action: String,
    },
    /// `stream.dropped` — how many events this subscriber lost to
    /// backpressure so far (cumulative).
    StreamDropped {
        /// The cumulative `dropped_events` count for the subscriber.
        dropped_events: u64,
    },
    /// `stream.gap` — the generation range that could not be
    /// replayed from retention.
    StreamGap {
        /// First missing generation (inclusive).
        from_generation: u64,
        /// Last missing generation (inclusive).
        to_generation: u64,
    },
}

impl EventData {
    /// The kind of event this payload carries.
    pub const fn kind(&self) -> EventKind {
        match self {
            Self::StateChange { .. } => EventKind::InstanceStateChange,
            Self::ConsoleOutput { .. } => EventKind::InstanceConsoleOutput,
            Self::ResourceUsage(_) => EventKind::InstanceResourceUsage,
            Self::InstallProgress { .. } => EventKind::InstanceInstallProgress,
            Self::JobProgress { .. } => EventKind::JobProgress,
            Self::JobCompleted { .. } => EventKind::JobCompleted,
            Self::NodeStateChange { .. } => EventKind::NodeStateChange,
            Self::AuditRecord { .. } => EventKind::AuditRecord,
            Self::StreamDropped { .. } => EventKind::StreamDropped,
            Self::StreamGap { .. } => EventKind::StreamGap,
        }
    }
}

/// The resource-usage sample carried by
/// `instance.resource_usage` events. Mirrors the supervision
/// module's runtime observation shape (the application layer maps
/// its `ResourceUsage` onto this wire form — the streaming crate
/// cannot depend on the application layer per the crate graph).
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct ResourceUsageData {
    /// Total CPU time consumed by the workload's processes
    /// (nanoseconds), when reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_usage_total_ns: Option<u64>,
    /// The host's total CPU time over the same measurement window
    /// (nanoseconds), for utilization math.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_cpu_total_ns: Option<u64>,
    /// Online CPUs visible to the workload, when reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub online_cpus: Option<u64>,
    /// Current memory usage in bytes, when reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_usage_bytes: Option<u64>,
    /// The memory limit in force, when reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_limit_bytes: Option<u64>,
    /// Current process count inside the workload, when reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pids_current: Option<u64>,
}

/// One streaming event (the in-engine form of the §11.1 envelope).
///
/// Meta-events (`stream.dropped`, `stream.gap`) are synthesized per
/// subscriber or per replay; they are not part of the per-instance
/// ordered log and carry `generation: 0` in their envelope, with the
/// affected range (gap) or count (dropped) in the payload.
#[derive(Debug, Clone)]
pub struct StreamEvent {
    /// The event's type discriminant.
    pub kind: EventKind,
    /// The per-instance monotonic event generation (0 for
    /// meta-events).
    pub generation: u64,
    /// When the engine produced the event.
    pub ts: DateTime<Utc>,
    /// The event's instance, when scoped to one (`None` for
    /// engine-global events).
    pub instance_id: Option<InstanceId>,
    /// The event-specific payload.
    pub data: EventData,
}

impl StreamEvent {
    /// Build a lane event (the hub is the only intended caller — it
    /// stamps the generation and timestamp).
    pub(crate) fn lane_event(
        generation: u64,
        ts: DateTime<Utc>,
        instance_id: Option<InstanceId>,
        data: EventData,
    ) -> Self {
        Self {
            kind: data.kind(),
            generation,
            ts,
            instance_id,
            data,
        }
    }

    /// Build a subscriber-scoped meta-event (generation 0 by
    /// convention: meta-events are not ordered against the lane
    /// log).
    pub(crate) fn meta_event(
        ts: DateTime<Utc>,
        instance_id: Option<InstanceId>,
        data: EventData,
    ) -> Self {
        Self {
            kind: data.kind(),
            generation: 0,
            ts,
            instance_id,
            data,
        }
    }

    /// The approximate serialized footprint of the console payload,
    /// used for the byte-bounded console retention window.
    pub(crate) fn console_bytes(&self) -> usize {
        match &self.data {
            EventData::ConsoleOutput { line } => line.len(),
            _ => 0,
        }
    }
}

impl Serialize for StreamEvent {
    /// Serialize as the §11.1 envelope:
    ///
    /// ```json
    /// {"type":"instance.state_change","generation":7,
    ///  "ts":"2026-10-13T15:46:09.123Z",
    ///  "instance_id":"…","data":{"from":"offline","to":"starting"}}
    /// ```
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut out = serializer.serialize_struct("StreamEvent", 5)?;
        out.serialize_field("type", self.kind.as_str())?;
        out.serialize_field("generation", &self.generation)?;
        out.serialize_field("ts", &self.ts)?;
        match self.instance_id {
            Some(id) => out.serialize_field("instance_id", &id)?,
            None => out.serialize_field("instance_id", &Option::<InstanceId>::None)?,
        }
        out.serialize_field("data", &self.data)?;
        out.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_strings_match_the_spec_vocabulary() {
        assert_eq!(
            EventKind::InstanceStateChange.as_str(),
            "instance.state_change"
        );
        assert_eq!(
            EventKind::InstanceConsoleOutput.as_str(),
            "instance.console_output"
        );
        assert_eq!(
            EventKind::InstanceResourceUsage.as_str(),
            "instance.resource_usage"
        );
        assert_eq!(EventKind::StreamDropped.as_str(), "stream.dropped");
        assert_eq!(EventKind::StreamGap.as_str(), "stream.gap");
    }

    #[test]
    fn envelope_serializes_in_spec_shape() {
        let event = StreamEvent {
            kind: EventKind::InstanceStateChange,
            generation: 7,
            ts: Utc::now(),
            instance_id: Some(InstanceId::new()),
            data: EventData::StateChange {
                from: InstanceStatus::Offline,
                to: InstanceStatus::Starting,
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "instance.state_change");
        assert_eq!(json["generation"], 7);
        assert!(json["ts"].as_str().is_some_and(|s| s.ends_with('Z')));
        assert!(json["instance_id"].as_str().is_some());
        assert_eq!(json["data"]["from"], "offline");
        assert_eq!(json["data"]["to"], "starting");

        let meta = StreamEvent {
            kind: EventKind::StreamGap,
            generation: 0,
            ts: Utc::now(),
            instance_id: None,
            data: EventData::StreamGap {
                from_generation: 3,
                to_generation: 9,
            },
        };
        let json = serde_json::to_value(&meta).unwrap();
        assert_eq!(json["type"], "stream.gap");
        assert_eq!(json["generation"], 0);
        assert!(json["instance_id"].is_null());
        assert_eq!(json["data"]["from_generation"], 3);
        assert_eq!(json["data"]["to_generation"], 9);
    }

    #[test]
    fn resource_usage_omits_unreported_fields() {
        let data = ResourceUsageData {
            memory_usage_bytes: Some(512),
            ..Default::default()
        };
        let json = serde_json::to_value(data).unwrap();
        assert_eq!(json["memory_usage_bytes"], 512);
        assert!(json.get("cpu_usage_total_ns").is_none());
    }
}
