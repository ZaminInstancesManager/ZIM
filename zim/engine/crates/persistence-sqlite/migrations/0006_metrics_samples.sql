-- M30 telemetry ingestion: the persisted resource-usage samples.
--
-- The monitor's `instance.resource_usage` observations at the
-- telemetry persist cadence (10 s by default — the stream keeps its
-- 1 s live cadence; history does not need per-second resolution and
-- the store should not carry it). One row per (instance, ts): raw
-- counters only — rates and percentages are derived at read time
-- from consecutive rows, never persisted (a stored rate would freeze
-- the denominator choice the window query should be free to make).
--
-- Retention is pruned by the telemetry service (7 days by default);
-- the ON DELETE CASCADE posture matches every per-instance table, so
-- a deleted instance's samples go with it.
--
-- Conventions (per 0001): UUID ids -> TEXT (canonical hyphenated),
-- timestamps -> TEXT in RFC 3339 / UTC, counters -> INTEGER (NULL =
-- unknown, never zero — the M17 network-accounting convention).

CREATE TABLE metrics_samples (
    instance_id        TEXT    NOT NULL REFERENCES instances (id) ON DELETE CASCADE,
    ts                 TEXT    NOT NULL,
    cpu_usage_total_ns  INTEGER,
    system_cpu_total_ns INTEGER,
    online_cpus        INTEGER,
    memory_usage_bytes INTEGER,
    memory_limit_bytes INTEGER,
    pids_current       INTEGER,
    network_rx_bytes   INTEGER,
    network_tx_bytes   INTEGER,
    PRIMARY KEY (instance_id, ts)
);

CREATE INDEX idx_metrics_samples_ts
    ON metrics_samples (instance_id, ts);
