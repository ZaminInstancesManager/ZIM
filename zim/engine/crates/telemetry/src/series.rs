//! The history series pipeline (M30): the pure math between the
//! store's raw rows and the chart's points.
//!
//! Everything here is a pure function over
//! [`MetricsSample`](zim_engine_domain::MetricsSample) slices — no
//! I/O, no clocks, no policy — so the derivation vocabulary
//! (counter deltas, rate and percentage math, downsampling) is
//! unit-testable without an engine. The query half (which rows to
//! fetch, over which window, with how much lookback) lives in
//! [`crate`]'s service; the assembly half lives here as
//! [`assemble_history`].
//!
//! # Pipeline order (why derive → cut → downsample)
//!
//! Derivation comes **first**, over consecutive raw rows: rates
//! computed at the samples' own spacing stay exact, and a series
//! whose every row falls inside one bucket still yields a point
//! with real rates (downsampling first would discard the
//! predecessor and turn the rates unknown). The window cut comes
//! second — the fetch's lookback rows exist precisely so the first
//! in-window point inherits a rate, and the cut drops their own
//! points without orphaning the ones that remain. Downsampling
//! comes last: the *last* derived point in each bucket survives,
//! which reads a gauge at the bucket's edge and a rate at the
//! sample's own instant — the standard sparkline discipline.
//!
//! # Derivation rules (the honest-series contract)
//!
//! - **Counters are cumulative.** CPU time and network byte counts
//!   only ever grow *for one container lifetime*. A restart resets
//!   them; a delta where the newer sample is *smaller* is a reset,
//!   and the derived rate for that interval is `None` (unknown), not
//!   a negative number.
//! - **Rates divide by the wall interval between the two samples**,
//!   taken from the rows' own timestamps — never from the persist
//!   cadence, which is a policy that can change under a running
//!   series.
//! - **`cpu_percent` is the host-share form**: workload CPU delta
//!   over host CPU delta, 0–100. Multi-core saturation shows as 100,
//!   not 400 — the "how much of the machine" question the chart
//!   asks. (Cores-busy is re-derivable from the raw counters, which
//!   ride along on every point.)
//! - **`None` means unknown.** A runtime that reports no network
//!   counters produces no network points — never a zero line that
//!   reads as "idle".

use chrono::{DateTime, Utc};
use serde::Serialize;
use zim_engine_domain::MetricsSample;

use crate::window::MetricsWindow;

/// How many points a rendered series carries at most — the chart's
/// pixel budget. A wider window means a wider bucket, not more
/// points.
pub const SERIES_POINT_BUDGET: usize = 180;

/// How far before the requested window the caller should fetch rows
/// so the first drawn point can derive its rate from a predecessor
/// (two persist-cadence intervals is comfortably more than one
/// sample at every supported cadence).
pub const WINDOW_LOOKBACK_SECS: i64 = 60;

/// One derived point of a history series.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct HistoryPoint {
    /// When the point's sample was taken.
    pub ts: DateTime<Utc>,
    /// Workload CPU as a share of host CPU over the preceding
    /// interval (0–100), when both counters are known.
    pub cpu_percent: Option<f64>,
    /// Memory usage at the sample (an instantaneous gauge).
    pub memory_usage_bytes: Option<u64>,
    /// The memory limit in force at the sample.
    pub memory_limit_bytes: Option<u64>,
    /// Memory usage as a share of the limit (0–100), when both are
    /// known and the limit is non-zero.
    pub memory_percent: Option<f64>,
    /// Process count at the sample.
    pub pids_current: Option<u64>,
    /// Receive rate over the preceding interval (bytes/second).
    pub network_rx_bytes_per_sec: Option<f64>,
    /// Transmit rate over the preceding interval (bytes/second).
    pub network_tx_bytes_per_sec: Option<f64>,
    /// The cumulative receive counter at the sample (raw, for
    /// re-derivation client-side).
    pub network_rx_bytes_total: Option<u64>,
    /// The cumulative transmit counter at the sample.
    pub network_tx_bytes_total: Option<u64>,
}

/// The summary statistics of one rendered series — the numbers a
/// header strip wants (avg/peak CPU, peak memory, transferred
/// totals) without re-walking the points client-side.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct HistorySummary {
    /// How many points the series carries.
    pub point_count: usize,
    /// The first point's timestamp (`None` for an empty series).
    pub first_ts: Option<DateTime<Utc>>,
    /// The last point's timestamp.
    pub last_ts: Option<DateTime<Utc>>,
    /// Mean CPU percent over the points that carry one.
    pub cpu_percent_avg: Option<f64>,
    /// Maximum CPU percent over the points that carry one.
    pub cpu_percent_max: Option<f64>,
    /// Peak memory usage in bytes.
    pub memory_bytes_max: Option<u64>,
    /// Total bytes received across the window (the sum of positive
    /// per-interval deltas — restart resets contribute nothing
    /// rather than a bogus negative total).
    pub network_rx_bytes_total: Option<u64>,
    /// Total bytes transmitted across the window.
    pub network_tx_bytes_total: Option<u64>,
    /// Peak receive rate (bytes/second).
    pub network_rx_bytes_per_sec_peak: Option<f64>,
    /// Peak transmit rate (bytes/second).
    pub network_tx_bytes_per_sec_peak: Option<f64>,
}

/// One rendered history series: the points plus the summary plus
/// the resolution the caller effectively got.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistorySeries {
    /// The surviving points, oldest first, at most
    /// [`SERIES_POINT_BUDGET`] plus one bucket boundary.
    pub points: Vec<HistoryPoint>,
    /// The series' summary statistics.
    pub summary: HistorySummary,
    /// The bucket width the series was downsampled to (seconds).
    pub resolution_seconds: i64,
}

/// Assemble the rendered series for one window from the fetched
/// raw rows (oldest first, including any lookback rows before the
/// window's start — the lookback exists so the first retained point
/// can derive its rate from a predecessor).
///
/// The pipeline, in order: derive points between consecutive raw
/// rows (the first row becomes a gauge-only point — its rates are
/// unknown, not zero), cut to `[window start, now]`, then keep the
/// last point of each resolution bucket.
pub fn assemble_history(
    window: MetricsWindow,
    samples: &[MetricsSample],
    now: DateTime<Utc>,
) -> HistorySeries {
    let window_start = now - chrono::Duration::seconds(window.seconds());
    let resolution = resolution_seconds(window);
    let bucket_of = |ts: DateTime<Utc>| ts.timestamp_millis() / (resolution * 1000);

    // ── Derive: every consecutive raw pair becomes a point ───────
    let mut points: Vec<HistoryPoint> = Vec::with_capacity(samples.len());
    if let Some(first) = samples.first() {
        points.push(gauge_only_point(first));
    }
    for pair in samples.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        points.push(HistoryPoint {
            ts: b.ts,
            cpu_percent: cpu_percent(a, b),
            memory_usage_bytes: b.memory_usage_bytes,
            memory_limit_bytes: b.memory_limit_bytes,
            memory_percent: memory_percent(b),
            pids_current: b.pids_current,
            network_rx_bytes_per_sec: rate_per_sec(a.network_rx_bytes, b.network_rx_bytes, a, b),
            network_tx_bytes_per_sec: rate_per_sec(a.network_tx_bytes, b.network_tx_bytes, a, b),
            network_rx_bytes_total: b.network_rx_bytes,
            network_tx_bytes_total: b.network_tx_bytes,
        });
    }

    // ── Cut: only points inside the window survive ───────────────
    // (also drops a clock-skewed row from the future — a point
    // beyond `now` must never render.)
    points.retain(|point| point.ts >= window_start && point.ts <= now);

    // ── Downsample: the last point of each bucket survives ───────
    let mut kept: Vec<HistoryPoint> = Vec::with_capacity(points.len());
    for point in points {
        match kept.last_mut() {
            Some(last) if bucket_of(last.ts) == bucket_of(point.ts) => *last = point,
            _ => kept.push(point),
        }
    }

    // ── Summarize ────────────────────────────────────────────────
    let summary = summarize(&kept);
    HistorySeries {
        points: kept,
        summary,
        resolution_seconds: resolution,
    }
}

/// The bucket width for one window: the window span over the point
/// budget, never narrower than one second (the floor guards absurd
/// configurations; every supported window already clears it).
pub fn resolution_seconds(window: MetricsWindow) -> i64 {
    (window.seconds() / SERIES_POINT_BUDGET as i64).max(1)
}

/// The gauge-only point for a series' first fetched row: everything
/// the sample itself reports, with rates unknown (a rate needs a
/// predecessor; inventing a zero baseline would read "idle").
fn gauge_only_point(sample: &MetricsSample) -> HistoryPoint {
    HistoryPoint {
        ts: sample.ts,
        cpu_percent: None,
        memory_usage_bytes: sample.memory_usage_bytes,
        memory_limit_bytes: sample.memory_limit_bytes,
        memory_percent: memory_percent(sample),
        pids_current: sample.pids_current,
        network_rx_bytes_per_sec: None,
        network_tx_bytes_per_sec: None,
        network_rx_bytes_total: sample.network_rx_bytes,
        network_tx_bytes_total: sample.network_tx_bytes,
    }
}

/// Workload-CPU-over-host-CPU percent for one interval, when both
/// cumulative counters are present and the host delta is positive.
/// A *negative* workload delta (a counter reset) is unknown, not a
/// negative percentage.
fn cpu_percent(a: &MetricsSample, b: &MetricsSample) -> Option<f64> {
    let workload = counter_delta(a.cpu_usage_total_ns, b.cpu_usage_total_ns)?;
    let host = counter_delta(a.system_cpu_total_ns, b.system_cpu_total_ns)?;
    if host == 0 {
        return None;
    }
    Some(((workload as f64 / host as f64) * 100.0).clamp(0.0, 100.0))
}

/// Memory usage as a share of the limit at one sample.
fn memory_percent(sample: &MetricsSample) -> Option<f64> {
    let usage = sample.memory_usage_bytes?;
    let limit = sample.memory_limit_bytes?;
    if limit == 0 {
        return None;
    }
    Some(((usage as f64 / limit as f64) * 100.0).clamp(0.0, 100.0))
}

/// A cumulative counter's delta across one interval, in bytes per
/// second. `None` when either side is unknown, when the newer value
/// is smaller (a reset), or when the interval is not positive.
fn rate_per_sec(
    before: Option<u64>,
    after: Option<u64>,
    a: &MetricsSample,
    b: &MetricsSample,
) -> Option<f64> {
    let delta = counter_delta(before, after)?;
    let millis = b.ts.signed_duration_since(a.ts).num_milliseconds();
    if millis <= 0 {
        return None;
    }
    Some(delta as f64 * 1000.0 / millis as f64)
}

/// The delta of one cumulative counter across two samples: `None`
/// when either side is unknown or when the counter went backwards
/// (a reset — the interval straddles a restart).
fn counter_delta(before: Option<u64>, after: Option<u64>) -> Option<u64> {
    match (before, after) {
        (Some(before), Some(after)) if after >= before => Some(after - before),
        _ => None,
    }
}

/// Summarize one rendered series.
fn summarize(points: &[HistoryPoint]) -> HistorySummary {
    let mut summary = HistorySummary {
        point_count: points.len(),
        first_ts: points.first().map(|point| point.ts),
        last_ts: points.last().map(|point| point.ts),
        ..HistorySummary::default()
    };
    let mut cpu_sum = 0.0;
    let mut cpu_count = 0usize;
    let mut rx_total: u64 = 0;
    let mut tx_total: u64 = 0;
    let mut rx_known = false;
    let mut tx_known = false;
    for (index, point) in points.iter().enumerate() {
        if let Some(cpu) = point.cpu_percent {
            cpu_sum += cpu;
            cpu_count += 1;
            summary.cpu_percent_max = Some(summary.cpu_percent_max.map_or(cpu, |max| max.max(cpu)));
        }
        if let Some(memory) = point.memory_usage_bytes {
            summary.memory_bytes_max = Some(
                summary
                    .memory_bytes_max
                    .map_or(memory, |max| max.max(memory)),
            );
        }
        if let Some(rate) = point.network_rx_bytes_per_sec {
            summary.network_rx_bytes_per_sec_peak = Some(
                summary
                    .network_rx_bytes_per_sec_peak
                    .map_or(rate, |peak| peak.max(rate)),
            );
        }
        if let Some(rate) = point.network_tx_bytes_per_sec {
            summary.network_tx_bytes_per_sec_peak = Some(
                summary
                    .network_tx_bytes_per_sec_peak
                    .map_or(rate, |peak| peak.max(rate)),
            );
        }
        // Transferred totals: the counters are cumulative, so the
        // delta between consecutive points' totals is exactly the
        // bytes transferred between them — no matter how many raw
        // rows the downsampling folded away. Resets (a smaller newer
        // total) contribute nothing.
        if index > 0 {
            let previous = &points[index - 1];
            if let (Some(before), Some(after)) = (
                previous.network_rx_bytes_total,
                point.network_rx_bytes_total,
            ) {
                if after >= before {
                    rx_total += after - before;
                    rx_known = true;
                }
            }
            if let (Some(before), Some(after)) = (
                previous.network_tx_bytes_total,
                point.network_tx_bytes_total,
            ) {
                if after >= before {
                    tx_total += after - before;
                    tx_known = true;
                }
            }
        }
    }
    if cpu_count > 0 {
        summary.cpu_percent_avg = Some(cpu_sum / cpu_count as f64);
    }
    if rx_known {
        summary.network_rx_bytes_total = Some(rx_total);
    }
    if tx_known {
        summary.network_tx_bytes_total = Some(tx_total);
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use zim_engine_domain::InstanceId;

    /// Build one raw sample at `t0 + seconds` with the given counters.
    fn sample(
        instance: InstanceId,
        t0: DateTime<Utc>,
        seconds: i64,
        cpu_ns: Option<u64>,
        system_ns: Option<u64>,
        memory: Option<u64>,
        rx: Option<u64>,
        tx: Option<u64>,
    ) -> MetricsSample {
        MetricsSample {
            instance_id: instance,
            ts: t0 + chrono::Duration::seconds(seconds),
            cpu_usage_total_ns: cpu_ns,
            system_cpu_total_ns: system_ns,
            online_cpus: Some(2),
            memory_usage_bytes: memory,
            memory_limit_bytes: memory.map(|usage| usage.saturating_mul(2)),
            pids_current: Some(3),
            network_rx_bytes: rx,
            network_tx_bytes: tx,
        }
    }

    /// A `now` aligned to the 20 s bucket grid (12:00:00), so
    /// bucket boundaries in the fixtures are exact.
    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 19, 12, 0, 0).unwrap()
    }

    fn id() -> InstanceId {
        InstanceId::from_uuid(uuid::Uuid::new_v4())
    }

    #[test]
    fn resolution_is_the_window_over_the_budget() {
        assert_eq!(resolution_seconds(MetricsWindow::Minutes15), 5);
        assert_eq!(resolution_seconds(MetricsWindow::Hour1), 20);
        assert_eq!(resolution_seconds(MetricsWindow::Hours24), 480);
        assert_eq!(resolution_seconds(MetricsWindow::Days7), 3360);
    }

    #[test]
    fn rates_and_percentages_derive_from_consecutive_rows() {
        let instance = id();
        let now = t0();
        // Two rows 5 s apart, both inside one 20 s bucket: +100 ms
        // CPU against +1 s host, +1000 B rx, +250 B tx, 4 MiB of
        // 8 MiB memory. Deriving before downsampling is what keeps
        // the single surviving point's rates real.
        let samples = vec![
            sample(
                instance,
                now,
                -5,
                Some(0),
                Some(0),
                Some(4 * 1024 * 1024),
                Some(0),
                Some(0),
            ),
            sample(
                instance,
                now,
                0,
                Some(100_000_000),
                Some(1_000_000_000),
                Some(4 * 1024 * 1024),
                Some(1000),
                Some(250),
            ),
        ];
        let series = assemble_history(MetricsWindow::Hour1, &samples, now);
        assert_eq!(series.points.len(), 1);
        let point = series.points[0];
        assert_eq!(point.ts, now);
        // 0.1 / 1.0 = 10 % of the host.
        assert_eq!(point.cpu_percent, Some(10.0));
        // 1000 B / 5 s = 200 B/s.
        assert_eq!(point.network_rx_bytes_per_sec, Some(200.0));
        assert_eq!(point.network_tx_bytes_per_sec, Some(50.0));
        // Memory reads 50 % of its limit.
        assert_eq!(point.memory_percent, Some(50.0));
        // The summary agrees.
        assert_eq!(series.summary.cpu_percent_avg, Some(10.0));
        assert_eq!(series.summary.network_rx_bytes_total, Some(1000));
        assert_eq!(series.summary.network_tx_bytes_total, Some(250));
        assert_eq!(series.summary.memory_bytes_max, Some(4 * 1024 * 1024));
    }

    #[test]
    fn downsampling_keeps_the_last_point_of_each_bucket() {
        let instance = id();
        let now = t0();
        // 1 h window -> 20 s buckets. Six rows at 0/4/8/12/16/19 s
        // past `now - 60` all land in one bucket; one at +25 lands
        // in the next. The surviving points are the 19 s and 25 s
        // ones.
        let base = now - chrono::Duration::seconds(60);
        let mut samples = Vec::new();
        for (second, counter) in [(0, 0), (4, 40), (8, 80), (12, 120), (16, 160), (19, 190)] {
            samples.push(sample(
                instance,
                base,
                second,
                Some(counter * 1_000_000),
                Some(counter * 10_000_000),
                Some(1024),
                Some(counter as u64 * 100),
                Some(0),
            ));
        }
        samples.push(sample(
            instance,
            base,
            25,
            Some(250_000_000),
            Some(2_500_000_000),
            Some(1024),
            Some(25_000),
            Some(0),
        ));
        let series = assemble_history(MetricsWindow::Hour1, &samples, now);
        assert_eq!(series.points.len(), 2);
        // The surviving points are the 19 s and 25 s ones (a point
        // keeps its own sample's timestamp, not the bucket's edge).
        assert_eq!(series.points[0].ts, base + chrono::Duration::seconds(19));
        assert_eq!(series.points[1].ts, base + chrono::Duration::seconds(25));
        // The 25 s point's rate spans the two surviving samples:
        // (25000 - 19000) B over 6 s = 100 B/s.
        assert_eq!(series.points[1].network_rx_bytes_per_sec, Some(100.0));
    }

    #[test]
    fn lookback_rows_seed_but_do_not_render() {
        let instance = id();
        let now = t0();
        // The 15-minute window starts 900 s back. One lookback row
        // at -950 s (fetched to seed the first rate) and two
        // in-window rows at -10 s and now.
        let samples = vec![
            sample(
                instance,
                now,
                -950,
                Some(0),
                Some(0),
                Some(1024),
                Some(0),
                Some(0),
            ),
            sample(
                instance,
                now,
                -10,
                Some(0),
                Some(0),
                Some(1024),
                Some(2000),
                Some(0),
            ),
            sample(
                instance,
                now,
                0,
                Some(0),
                Some(0),
                Some(1024),
                Some(4000),
                Some(0),
            ),
        ];
        let series = assemble_history(MetricsWindow::Minutes15, &samples, now);
        // The lookback row's own point is cut; the first in-window
        // point inherits its rate (2000 B over 940 s).
        assert_eq!(series.points.len(), 2);
        assert!(series.points[0].ts >= now - chrono::Duration::seconds(900));
        assert!(series.points[0].network_rx_bytes_per_sec.is_some());
        assert_eq!(series.points[1].network_rx_bytes_per_sec, Some(200.0));
    }

    #[test]
    fn the_first_fetched_row_renders_as_a_gauge_only_point() {
        let instance = id();
        let now = t0();
        // No lookback fetch: the series' first row is in-window. It
        // renders with its gauges and unknown rates — never a zero
        // baseline that would read "idle".
        let samples = vec![
            sample(
                instance,
                now,
                -40,
                Some(0),
                Some(0),
                Some(2048),
                Some(500),
                Some(0),
            ),
            sample(
                instance,
                now,
                -20,
                Some(0),
                Some(0),
                Some(2048),
                Some(900),
                Some(0),
            ),
        ];
        let series = assemble_history(MetricsWindow::Hour1, &samples, now);
        assert_eq!(series.points.len(), 2);
        assert_eq!(series.points[0].ts, now - chrono::Duration::seconds(40));
        assert_eq!(series.points[0].network_rx_bytes_per_sec, None);
        assert_eq!(series.points[0].cpu_percent, None);
        assert_eq!(series.points[0].memory_usage_bytes, Some(2048));
        // The second point derives normally (400 B over 20 s).
        assert_eq!(series.points[1].network_rx_bytes_per_sec, Some(20.0));
    }

    #[test]
    fn a_counter_reset_is_unknown_not_negative() {
        let instance = id();
        let now = t0();
        // 15-minute window (5 s buckets) so the three rows land in
        // three buckets and all survive. The rx counter resets to 0
        // at t+5 (a restart); the cpu workload counter resets at
        // t+10.
        let base = now - chrono::Duration::seconds(60);
        let samples = vec![
            sample(
                instance,
                base,
                0,
                Some(500),
                Some(1_000),
                Some(1024),
                Some(5000),
                Some(0),
            ),
            sample(
                instance,
                base,
                5,
                Some(600),
                Some(2_000),
                Some(1024),
                Some(0),
                Some(0),
            ),
            sample(
                instance,
                base,
                10,
                Some(100),
                Some(3_000),
                Some(1024),
                Some(100),
                Some(0),
            ),
        ];
        let series = assemble_history(MetricsWindow::Minutes15, &samples, now);
        assert_eq!(series.points.len(), 3);
        // +5 s: rx reset -> None; cpu 100/1000 -> 10 %.
        assert_eq!(series.points[1].network_rx_bytes_per_sec, None);
        assert_eq!(series.points[1].cpu_percent, Some(10.0));
        // +10 s: cpu reset -> None; rx 100 B over 5 s = 20 B/s.
        assert_eq!(series.points[2].cpu_percent, None);
        assert_eq!(series.points[2].network_rx_bytes_per_sec, Some(20.0));
        // The transferred total counts only the known interval.
        assert_eq!(series.summary.network_rx_bytes_total, Some(100));
    }

    #[test]
    fn an_empty_series_is_honestly_empty() {
        let series = assemble_history(MetricsWindow::Hour1, &[], t0());
        assert!(series.points.is_empty());
        assert_eq!(series.summary.point_count, 0);
        assert_eq!(series.summary.first_ts, None);
        assert_eq!(series.summary.cpu_percent_avg, None);
        assert_eq!(series.resolution_seconds, 20);
    }

    #[test]
    fn missing_counters_never_become_zero() {
        let instance = id();
        let now = t0();
        // A runtime that reports no network counters and no CPU
        // counters: only the gauges survive.
        let samples = vec![
            sample(instance, now, -10, None, None, Some(2048), None, None),
            sample(instance, now, 0, None, None, Some(4096), None, None),
        ];
        let series = assemble_history(MetricsWindow::Hour1, &samples, now);
        assert_eq!(series.points.len(), 1);
        assert_eq!(series.points[0].cpu_percent, None);
        assert_eq!(series.points[0].network_rx_bytes_per_sec, None);
        assert_eq!(series.points[0].memory_usage_bytes, Some(4096));
        assert_eq!(series.points[0].memory_percent, Some(50.0));
        assert_eq!(series.summary.memory_bytes_max, Some(4096));
        assert_eq!(series.summary.network_rx_bytes_total, None);
    }

    #[test]
    fn future_rows_are_cut_too() {
        let instance = id();
        let now = t0();
        // A row beyond `now` (a clock skew or a race) must never
        // render into the window.
        let samples = vec![
            sample(
                instance,
                now,
                -40,
                Some(0),
                Some(0),
                Some(1024),
                Some(0),
                Some(0),
            ),
            sample(
                instance,
                now,
                -20,
                Some(0),
                Some(0),
                Some(1024),
                Some(100),
                Some(0),
            ),
            sample(
                instance,
                now,
                30,
                Some(0),
                Some(0),
                Some(1024),
                Some(200),
                Some(0),
            ),
        ];
        let series = assemble_history(MetricsWindow::Hour1, &samples, now);
        assert_eq!(series.points.len(), 2);
        assert!(series.points.iter().all(|point| point.ts <= now));
    }
}
