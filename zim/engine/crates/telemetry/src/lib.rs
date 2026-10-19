//! `zim_engine_telemetry` — the telemetry ingestion module (M30).
//!
//! The architecture's crate register listed this module as
//! "telemetry (TBD, opt-in)"; the post-alpha backlog named the gap
//! precisely: *"Telemetry ingestion — the module is specified; the
//! engine phones nobody home."* M30 delivers the specified half
//! without changing the second: **ingestion is local**. The
//! supervision monitor's `instance.resource_usage` observations are
//! persisted into the engine's own store at a lower cadence than
//! the live stream (10 s vs. 1 s), retained under a bounded window,
//! and served back as derived history series for the metrics
//! surface's charts. Nothing leaves the machine — the engine still
//! phones nobody home (see AD-34).
//!
//! # Shape
//!
//! - [`MetricsWindow`] — the query window vocabulary (`15m` … `7d`).
//! - [`assemble_history`] — the pure pipeline from raw rows to a
//!   rendered [`HistorySeries`] (derivation, window cut,
//!   downsampling; see `series.rs` for the rules).
//! - [`TelemetryService`] — the process-lifetime half: the history
//!   query (read + assemble behind one call) and the retention loop
//!   that prunes rows older than the policy's window.
//! - [`TelemetryOptions`] — the env-shaped knobs
//!   (`ZIM_METRICS_RETENTION_HOURS`, `ZIM_METRICS_PRUNE_INTERVAL_\
//! SECS`, `ZIM_METRICS_PERSIST_SECS`), consumed by the composition
//!   root.
//!
//! # Ingestion is not here
//!
//! The write path lives in the supervision layer
//! (`zim_engine_application`), where the monitor already samples the
//! runtime: persisting from there avoids a second sampler and keeps
//! the crate graph acyclic (this crate depends on the application
//! layer's `Persistence` port; the application layer must not
//! depend on this one). The store family is the port's
//! `append_metrics_sample` / `list_metrics_history` /
//! `prune_metrics_before` triple.
//!
//! # Architecture boundary
//!
//! This crate's role, dependencies, and permitted callers are
//! documented in `docs/architecture/RUST_ENGINE.md` (the deferred
//! `telemetry/` crate, delivered). The permitted dependency
//! direction is enforced at compile time by the workspace crate
//! graph.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use zim_engine_application::ports::{Persistence, PersistenceError};
use zim_engine_domain::{InstanceId, MetricsSample};

pub mod series;
pub mod window;

pub use series::{
    assemble_history, resolution_seconds, HistoryPoint, HistorySeries, HistorySummary,
    SERIES_POINT_BUDGET, WINDOW_LOOKBACK_SECS,
};
pub use window::MetricsWindow;

/// The telemetry error surface.
#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    /// The store read or prune failed.
    #[error("telemetry store error: {0}")]
    Store(#[from] PersistenceError),
}

/// How many raw rows one history query may read (the window's rows
/// at the persist cadence, plus generous headroom for a tighter
/// cadence or a clock-skewed writer).
pub const HISTORY_ROW_LIMIT: u32 = 20_000;

/// Process-lifetime telemetry policy: the retention window and the
/// prune cadence.
#[derive(Debug, Clone)]
pub struct TelemetryPolicy {
    /// How long samples live in the store. The default (7 days)
    /// matches the widest query window; a shorter retention
    /// honestly narrows the `7d` view's answer instead of failing.
    pub retention: Duration,
    /// How often the background prune runs (the first pass runs at
    /// service start, immediately after boot).
    pub prune_interval: Duration,
}

impl Default for TelemetryPolicy {
    fn default() -> Self {
        Self {
            retention: Duration::from_secs(7 * 24 * 3600),
            prune_interval: Duration::from_secs(3600),
        }
    }
}

/// The engine-option surface for telemetry (env-shaped; the
/// composition root parses these once at boot).
#[derive(Debug, Clone)]
pub struct TelemetryOptions {
    /// The retention window (see [`TelemetryPolicy::retention`]).
    pub retention: Duration,
    /// The prune cadence (see [`TelemetryPolicy::prune_interval`]).
    pub prune_interval: Duration,
    /// How often the supervision monitor persists a sample (the
    /// `metrics_persist_interval` policy knob; `ZERO` disables
    /// persistence entirely).
    pub persist_interval: Duration,
}

impl Default for TelemetryOptions {
    fn default() -> Self {
        let policy = TelemetryPolicy::default();
        Self {
            retention: policy.retention,
            prune_interval: policy.prune_interval,
            persist_interval: Duration::from_secs(10),
        }
    }
}

impl TelemetryOptions {
    /// Fill unset values from the environment:
    /// `ZIM_METRICS_RETENTION_HOURS` (default 168),
    /// `ZIM_METRICS_PRUNE_INTERVAL_SECS` (default 3600), and
    /// `ZIM_METRICS_PERSIST_SECS` (default 10; `0` disables
    /// persistence) — an unparsable or non-positive value keeps
    /// the default (the boot log says so), never refuses the boot.
    pub fn from_env(base: Self) -> Self {
        let mut opts = base;
        if let Ok(text) = std::env::var("ZIM_METRICS_RETENTION_HOURS") {
            if let Ok(hours) = text.trim().parse::<u64>() {
                opts.retention = Duration::from_secs(hours.saturating_mul(3600));
            }
        }
        if let Ok(text) = std::env::var("ZIM_METRICS_PRUNE_INTERVAL_SECS") {
            if let Ok(secs) = text.trim().parse::<u64>() {
                if secs > 0 {
                    opts.prune_interval = Duration::from_secs(secs);
                }
            }
        }
        if let Ok(text) = std::env::var("ZIM_METRICS_PERSIST_SECS") {
            if let Ok(secs) = text.trim().parse::<u64>() {
                opts.persist_interval = Duration::from_secs(secs);
            }
        }
        opts
    }

    /// The process-lifetime policy these options reduce to.
    pub fn policy(&self) -> TelemetryPolicy {
        TelemetryPolicy {
            retention: self.retention,
            prune_interval: self.prune_interval,
        }
    }
}

/// The telemetry service: the history query and the retention loop
/// over one persistence handle. Cheap to hold; the composition root
/// builds one and calls [`TelemetryService::start`] once per boot.
pub struct TelemetryService {
    persistence: Arc<dyn Persistence>,
    policy: TelemetryPolicy,
}

impl TelemetryService {
    /// Build the service.
    pub fn new(persistence: Arc<dyn Persistence>, policy: TelemetryPolicy) -> Arc<Self> {
        Arc::new(Self {
            persistence,
            policy,
        })
    }

    /// The raw rows one window query fetches: from the window's
    /// start minus the derivation lookback, clamped to the
    /// retention edge — when the policy retains less than the
    /// window asks for, the query says what it has instead of
    /// pretending the older rows were never written. Oldest first,
    /// bounded by [`HISTORY_ROW_LIMIT`].
    fn fetch_window_rows(
        &self,
        instance: &InstanceId,
        window: MetricsWindow,
        now: DateTime<Utc>,
    ) -> Result<Vec<MetricsSample>, TelemetryError> {
        let mut since = now - chrono::Duration::seconds(window.seconds() + WINDOW_LOOKBACK_SECS);
        let retention_secs = self.policy.retention.as_secs() as i64;
        if retention_secs > 0 {
            let retention_edge = now - chrono::Duration::seconds(retention_secs);
            if since < retention_edge {
                since = retention_edge;
            }
        }
        Ok(self
            .persistence
            .list_metrics_history(instance, since, now, HISTORY_ROW_LIMIT)?)
    }

    /// Read and assemble one instance's history for one window.
    ///
    /// The store read is synchronous (the M3 port discipline); the
    /// API layer bridges it with `spawn_blocking` when it calls
    /// from an async context — see `zim_engine_api`'s handler.
    pub fn history(
        &self,
        instance: &InstanceId,
        window: MetricsWindow,
        now: DateTime<Utc>,
    ) -> Result<HistorySeries, TelemetryError> {
        let rows = self.fetch_window_rows(instance, window, now)?;
        Ok(assemble_history(window, &rows, now))
    }

    /// One retention pass: delete every sample older than the
    /// policy's window. Returns how many rows were removed (the
    /// boot pass logs it; tests assert it). Synchronous — the
    /// store is the M3 sync port; the loop below bridges it onto
    /// the async runtime.
    pub fn enforce_retention(&self) -> Result<u64, TelemetryError> {
        let cutoff = Utc::now() - chrono::Duration::seconds(self.policy.retention.as_secs() as i64);
        Ok(self.persistence.prune_metrics_before(cutoff)?)
    }

    /// Spawn the retention loop (call once per process, after the
    /// store is migrated). The first pass runs immediately — a
    /// fresh boot is exactly when a previous run's overdue rows
    /// deserve pruning — then on the policy's cadence for the
    /// process lifetime, mirroring the scheduler's loop shape.
    /// Each pass rides `spawn_blocking` (the sync-port bridge); a
    /// failed or panicking pass logs and waits for the next one —
    /// retention is housekeeping, never a boot or request
    /// dependency.
    pub fn start(self: &Arc<Self>) {
        let service = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                let pass = {
                    let service = Arc::clone(&service);
                    tokio::task::spawn_blocking(move || service.enforce_retention()).await
                };
                match pass {
                    Ok(Ok(0)) => {}
                    Ok(Ok(removed)) => eprintln!(
                        "zim_engine: telemetry: pruned {removed} sample row(s) past the \
                         retention window"
                    ),
                    Ok(Err(e)) => {
                        eprintln!("zim_engine: telemetry: retention pass failed: {e}");
                    }
                    Err(e) => {
                        eprintln!("zim_engine: telemetry: retention pass panicked: {e}");
                    }
                }
                tokio::time::sleep(service.policy.prune_interval).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// The in-memory store the retention + history tests share.
    fn store() -> Arc<dyn Persistence> {
        let store: Arc<dyn Persistence> =
            Arc::new(zim_engine_persistence_sqlite::SqlitePersistence::open_in_memory().unwrap());
        store.migrate().unwrap();
        store
    }

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 19, 12, 0, 0).unwrap()
    }

    /// Seed the FK chain a metrics row hangs from (node -> user ->
    /// allocation -> instance) and return the instance id. The
    /// samples table carries the per-instance ON DELETE CASCADE
    /// posture, so a bare id would trip the foreign key; `index`
    /// varies the unique columns (user email, allocation port) so
    /// one store can carry several seeded instances.
    fn seed_instance(store: &dyn Persistence, index: u16) -> InstanceId {
        use zim_engine_domain::{Instance, Node, User};
        let node = Node {
            id: zim_engine_domain::NodeId::new(),
            name: format!("telemetry-test-{index}"),
            location_id: zim_engine_domain::LocationId::new(),
            fqdn: "localhost".to_owned(),
            scheme: zim_engine_domain::Scheme::Https,
            public_port: 8443,
            listen_port: 8444,
            capabilities: Default::default(),
            deployment: zim_engine_domain::NodeDeployment::LocalLinux,
            last_heartbeat: None,
            last_seen_state: Default::default(),
        };
        store.put_node(&node).unwrap();
        let user = User {
            id: zim_engine_domain::UserId::new(),
            email: format!("telemetry-{index}@example.org"),
            username: format!("telemetry-{index}"),
            external_id: None,
            password_hash: None,
            oauth_subject: None,
            is_admin: false,
            root_admin: false,
        };
        store.put_user(&user).unwrap();
        let allocation_id = store
            .create_allocation(&node.id, "127.0.0.1".parse().unwrap(), 25_700 + index, None)
            .unwrap();
        let (uuid, uuid_short) = Instance::legacy_uuid_pair();
        let instance = Instance {
            id: InstanceId::new(),
            external_id: None,
            uuid,
            uuid_short,
            node_id: node.id,
            name: "Telemetry Test".to_owned(),
            description: String::new(),
            status: zim_engine_domain::InstanceStatus::Offline,
            skip_egg_scripts: false,
            owner_id: user.id,
            limits: Default::default(),
            allocation_id,
            egg_id: zim_engine_domain::EggId::new(),
            startup: zim_engine_domain::CommandTemplate::new("sleep 600"),
            image: "busybox:latest".parse().unwrap(),
            allocation_limit: None,
            database_limit: None,
            backup_limit: 0,
            crash_detection_enabled: true,
            installed_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        store.put_instance(&instance).unwrap();
        instance.id
    }

    #[test]
    fn history_reads_only_the_requested_instance() {
        let persistence = store();
        let a = seed_instance(persistence.as_ref(), 1);
        let b = seed_instance(persistence.as_ref(), 2);
        for instance in [a, b] {
            persistence
                .append_metrics_sample(&MetricsSample {
                    instance_id: instance,
                    ts: t0() - chrono::Duration::seconds(30),
                    memory_usage_bytes: Some(1024),
                    memory_limit_bytes: Some(2048),
                    pids_current: Some(2),
                    ..MetricsSample::default()
                })
                .unwrap();
        }
        let service = TelemetryService::new(Arc::clone(&persistence), TelemetryPolicy::default());
        let series = service.history(&a, MetricsWindow::Hour1, t0()).unwrap();
        // One row -> one gauge-only point (no predecessor to derive
        // from, so the rates are None — see series.rs).
        assert_eq!(series.points.len(), 1);
        assert_eq!(series.points[0].memory_usage_bytes, Some(1024));
        assert_eq!(series.points[0].network_rx_bytes_per_sec, None);
    }

    #[test]
    fn retention_prunes_only_older_rows() {
        let persistence = store();
        let instance = seed_instance(persistence.as_ref(), 1);
        // A 1-hour retention policy; rows at now-30 s (kept) and
        // now-2 h (pruned). The rows are placed against the real
        // clock because the prune cutoff is real-now — a fixed
        // fixture timestamp would race the runner's clock.
        let now = Utc::now();
        for (offset, memory) in [(-30, 1024), (-7200, 512)] {
            persistence
                .append_metrics_sample(&MetricsSample {
                    instance_id: instance,
                    ts: now + chrono::Duration::seconds(offset),
                    memory_usage_bytes: Some(memory),
                    ..MetricsSample::default()
                })
                .unwrap();
        }
        let service = TelemetryService::new(
            Arc::clone(&persistence),
            TelemetryPolicy {
                retention: Duration::from_secs(3600),
                prune_interval: Duration::from_secs(3600),
            },
        );
        let removed = service.enforce_retention().unwrap();
        assert_eq!(removed, 1);
        let rows = persistence
            .list_metrics_history(
                &instance,
                now - chrono::Duration::hours(3),
                now + chrono::Duration::hours(1),
                100,
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].memory_usage_bytes, Some(1024));
    }

    #[test]
    fn append_replaces_the_same_timestamp_row() {
        let persistence = store();
        let instance = seed_instance(persistence.as_ref(), 1);
        let ts = t0();
        persistence
            .append_metrics_sample(&MetricsSample {
                instance_id: instance,
                ts,
                memory_usage_bytes: Some(1024),
                ..MetricsSample::default()
            })
            .unwrap();
        // The same (instance, ts) key: the re-observation replaces,
        // it does not duplicate.
        persistence
            .append_metrics_sample(&MetricsSample {
                instance_id: instance,
                ts,
                memory_usage_bytes: Some(4096),
                ..MetricsSample::default()
            })
            .unwrap();
        let rows = persistence
            .list_metrics_history(&instance, ts - chrono::Duration::seconds(1), ts, 10)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].memory_usage_bytes, Some(4096));
    }

    #[test]
    fn a_narrower_retention_shortens_wide_windows_honestly() {
        let persistence = store();
        let instance = seed_instance(persistence.as_ref(), 1);
        // Rows across the last 24 h; retention of 2 h.
        for hour in [1, 3, 12, 23] {
            persistence
                .append_metrics_sample(&MetricsSample {
                    instance_id: instance,
                    ts: t0() - chrono::Duration::hours(hour),
                    memory_usage_bytes: Some(1024 * hour as u64),
                    ..MetricsSample::default()
                })
                .unwrap();
        }
        let service = TelemetryService::new(
            Arc::clone(&persistence),
            TelemetryPolicy {
                retention: Duration::from_secs(2 * 3600),
                prune_interval: Duration::from_secs(3600),
            },
        );
        let now = t0();
        let series = service
            .history(&instance, MetricsWindow::Days7, now)
            .unwrap();
        // Only the row inside the retention window renders; the
        // series says what it has, not what the window asked for.
        assert_eq!(series.points.len(), 1);
        assert_eq!(series.points[0].memory_usage_bytes, Some(1024));
    }

    #[test]
    fn default_options_match_the_documented_knobs() {
        let opts = TelemetryOptions::default();
        assert_eq!(opts.retention, Duration::from_secs(7 * 24 * 3600));
        assert_eq!(opts.prune_interval, Duration::from_secs(3600));
        assert_eq!(opts.persist_interval, Duration::from_secs(10));
    }

    #[test]
    fn the_documented_knobs_are_parsed_from_env_when_set() {
        // Tests within one process share the environment; these keys
        // are unique to this test and un-set elsewhere (the same
        // discipline the SFTP options' tests use).
        std::env::set_var("ZIM_METRICS_RETENTION_HOURS", "48");
        std::env::set_var("ZIM_METRICS_PRUNE_INTERVAL_SECS", "120");
        std::env::set_var("ZIM_METRICS_PERSIST_SECS", "2");
        let opts = TelemetryOptions::from_env(TelemetryOptions::default());
        std::env::remove_var("ZIM_METRICS_RETENTION_HOURS");
        std::env::remove_var("ZIM_METRICS_PRUNE_INTERVAL_SECS");
        std::env::remove_var("ZIM_METRICS_PERSIST_SECS");
        assert_eq!(opts.retention, Duration::from_secs(48 * 3600));
        assert_eq!(opts.prune_interval, Duration::from_secs(120));
        assert_eq!(opts.persist_interval, Duration::from_secs(2));
    }
}
