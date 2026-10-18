/**
 * The metrics-surface pure helpers (M30): the chart geometry (value
 * → pixel mapping, the unknown-is-a-gap rule, nice axis bounds) and
 * the rate/percent derivations the live strip and history charts
 * share. Everything here is DOM-free — the canvas drawing itself is
 * covered by the engine's page tests and the E2E walk.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { formatRate, METRICS_WINDOWS } from "../src/api.js";
import {
  buildChartModel,
  drawableRuns,
  gridlineValues,
  liveCpuPercent,
  niceCeiling,
} from "../src/charts.js";

test("niceCeiling rounds up to the 1/2/5 ladder", () => {
  assert.equal(niceCeiling(0), 0);
  assert.equal(niceCeiling(-5), 0);
  assert.equal(niceCeiling(1), 1);
  assert.equal(niceCeiling(1.2), 2);
  assert.equal(niceCeiling(2.5), 5);
  assert.equal(niceCeiling(6), 10);
  assert.equal(niceCeiling(73), 100);
  assert.equal(niceCeiling(180), 200);
  assert.equal(niceCeiling(900), 1000);
  // The ladder is decimal (1/2/5 × 10^k), not binary.
  assert.equal(niceCeiling(1024 * 1024 * 1.3), 2_000_000);
});

test("buildChartModel maps values onto the padded box", () => {
  // width 100, height 50, padding 10 → inner box x [10, 90],
  // y [10, 40]. Values 0 and 100 against yMax 100: baseline and
  // ceiling.
  const model = buildChartModel([0, 50, 100], 100, 50, 10, 100);
  assert.equal(model.yMax, 100);
  assert.deepEqual(model.vertices, [
    { x: 10, y: 40 },
    { x: 50, y: 25 },
    { x: 90, y: 10 },
  ]);
});

test("buildChartModel spreads a single-point series without dividing", () => {
  // One value: no step division happens (the NaN guard).
  const model = buildChartModel([42], 100, 50, 10, 100);
  assert.equal(model.vertices.length, 1);
  assert.equal(model.vertices[0].x, 10);
  assert.ok(model.vertices[0].y !== null && model.vertices[0].y > 10);
});

test("unknown values are gap markers, never zero points", () => {
  const model = buildChartModel([10, null, 30, null, 50], 100, 50, 10, 100);
  // The null vertices carry no y: nothing draws "idle" for unknown.
  assert.equal(model.vertices[1].y, null);
  assert.equal(model.vertices[3].y, null);
  // Malformed numbers (NaN / Infinity from a broken payload) are
  // gaps too, not geometry corruption.
  const hostile = buildChartModel([Number.NaN, 10, Number.POSITIVE_INFINITY], 100, 50, 10, 100);
  assert.equal(hostile.vertices[0].y, null);
  assert.equal(hostile.vertices[2].y, null);
});

test("an all-zero series sits on the flat baseline", () => {
  const model = buildChartModel([0, 0, 0], 100, 50, 10);
  assert.equal(model.yMax, 0);
  for (const vertex of model.vertices) {
    assert.equal(vertex.y, 40); // the bottom of the padded box
  }
});

test("values clamp to the axis instead of escaping the box", () => {
  const model = buildChartModel([150, 50], 100, 50, 10, 100);
  // 150 over a 0-100 axis draws at the ceiling, not above it.
  assert.equal(model.vertices[0].y, 10);
  assert.equal(model.vertices[1].y, 25);
});

test("drawableRuns breaks at gaps and drops single-point runs", () => {
  const model = buildChartModel([1, 2, null, null, 5, 6, 7, null, 9], 200, 50, 5, 10);
  const runs = drawableRuns(model.vertices);
  // [1,2] contiguous; [5,6,7] contiguous; the lone 9 is dropped
  // (a line needs two points).
  assert.deepEqual(runs, [[0, 1], [4, 5, 6]]);
  // An all-unknown series has no drawable runs at all.
  const empty = buildChartModel([null, null], 200, 50, 5, 10);
  assert.deepEqual(drawableRuns(empty.vertices), []);
});

test("gridlineValues spans zero to the ceiling", () => {
  assert.deepEqual(gridlineValues(100, 5), [0, 25, 50, 75, 100]);
  assert.deepEqual(gridlineValues(200, 3), [0, 100, 200]);
  // Degenerate axes collapse to the single bound.
  assert.deepEqual(gridlineValues(0, 5), [0]);
});

test("liveCpuPercent derives the host-share from stream deltas", () => {
  // +100 ms workload CPU against +1 s host CPU = 10 %.
  const before = { cpu_usage_total_ns: 0, system_cpu_total_ns: 0 };
  const after = { cpu_usage_total_ns: 100_000_000, system_cpu_total_ns: 1_000_000_000 };
  const cpu = liveCpuPercent(before, after);
  assert.ok(cpu !== null);
  assert.ok(Math.abs(cpu - 10) < 0.01);
  // A counter reset (a restart) is unknown, not negative.
  assert.equal(
    liveCpuPercent(
      { cpu_usage_total_ns: 500, system_cpu_total_ns: 1000 },
      { cpu_usage_total_ns: 100, system_cpu_total_ns: 2000 },
    ),
    null,
  );
  // Missing counters, or a zero host delta, are unknown too.
  assert.equal(liveCpuPercent({}, { cpu_usage_total_ns: 5 }), null);
  assert.equal(
    liveCpuPercent(
      { cpu_usage_total_ns: 0, system_cpu_total_ns: 0 },
      { cpu_usage_total_ns: 0, system_cpu_total_ns: 0 },
    ),
    null,
  );
});

test("formatRate renders byte rates with the shared convention", () => {
  assert.equal(formatRate(0), "0 B/s");
  assert.equal(formatRate(512), "512 B/s");
  assert.equal(formatRate(2048), "2.0 KiB/s");
  assert.equal(formatRate(1024 * 1024 * 3.5), "3.5 MiB/s");
});

test("the window vocabulary matches the engine's §10.3a ladder", () => {
  assert.deepEqual([...METRICS_WINDOWS], ["15m", "1h", "6h", "24h", "7d"]);
});
