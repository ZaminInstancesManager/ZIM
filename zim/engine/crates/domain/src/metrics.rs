//! Resource-usage samples: the persisted telemetry rows (M30).
//!
//! The supervision monitor samples the runtime's `stats` capability
//! every poll tick (1 s by default) and publishes each sample as an
//! `instance.resource_usage` stream event — the live view. M30 adds
//! the *durable* half of that pipeline: a lower-cadence ingest of the
//! same observations into the engine's store, so a chart can ask for
//! the last hour (or day, or week) after the fact. One row per
//! (instance, timestamp): the raw counters exactly as the runtime
//! reported them, cumulative where the runtime reports cumulative
//! (CPU time, network byte counts) and instantaneous where it
//! reports gauges (memory, pid count). Rates and percentages are
//! *derived* at read time from consecutive rows (see
//! `zim_engine_telemetry`) — they are never persisted, because a
//! persisted rate would freeze the denominator choice the window
//! query should be free to make.
//!
//! Retention is the store's job, not the row's: the telemetry
//! service prunes rows older than its retention window (7 days by
//! default) on a background cadence, and the table carries the same
//! `ON DELETE CASCADE` posture every per-instance table does — a
//! deleted instance's samples go with it.

use chrono::{DateTime, Utc};

use crate::id::InstanceId;

/// One persisted resource-usage sample for one instance.
///
/// The field vocabulary mirrors the supervision layer's
/// [`ResourceUsage`](crate::instance) observation shape (which in
/// turn mirrors what the Docker stats API reports); every field is
/// optional because every runtime reports a different subset —
/// `None` means "unknown", never "zero" (the M17 convention for
/// network accounting, applied uniformly here).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MetricsSample {
    /// The instance the sample observes.
    pub instance_id: InstanceId,
    /// When the runtime produced the sample (RFC 3339, UTC, at
    /// rest; the monitor's observation time, not the write time).
    pub ts: DateTime<Utc>,
    /// Total CPU time consumed by the workload's processes
    /// (nanoseconds), when reported — a *cumulative* counter.
    pub cpu_usage_total_ns: Option<u64>,
    /// The host's total CPU time over the same measurement window
    /// (nanoseconds), for utilization math — cumulative.
    pub system_cpu_total_ns: Option<u64>,
    /// Online CPUs visible to the workload, when reported.
    pub online_cpus: Option<u64>,
    /// Current memory usage in bytes (an instantaneous gauge).
    pub memory_usage_bytes: Option<u64>,
    /// The memory limit in force (an instantaneous gauge; matches
    /// the instance limits when one is set).
    pub memory_limit_bytes: Option<u64>,
    /// Current process count inside the workload (a gauge).
    pub pids_current: Option<u64>,
    /// Cumulative network bytes received across the workload's
    /// interfaces, when reported.
    pub network_rx_bytes: Option<u64>,
    /// Cumulative network bytes transmitted across the workload's
    /// interfaces, when reported.
    pub network_tx_bytes: Option<u64>,
}
