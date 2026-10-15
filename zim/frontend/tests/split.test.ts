/**
 * Unit tests for the split-view model (§8): ratio clamping, Chrome's
 * snap points, and the pointer→ratio math the divider drag uses.
 */

import test from "node:test";
import assert from "node:assert/strict";

import {
  SPLIT_MIN_RATIO,
  SPLIT_SNAP_POINTS,
  SPLIT_SNAP_TOLERANCE,
  clampRatio,
  isMirroredSplit,
  ratioFromPointer,
  snapRatio,
} from "../src/split";

/** A stand-in DOMRect (the pure math only reads left + width). */
function rect(left: number, width: number): DOMRect {
  return { left, width } as DOMRect;
}

test("clampRatio keeps both panes usable", () => {
  assert.equal(clampRatio(0.5), 0.5);
  assert.equal(clampRatio(0.2), SPLIT_MIN_RATIO);
  assert.equal(clampRatio(0.8), 0.8);
  assert.equal(clampRatio(0.05), SPLIT_MIN_RATIO);
  assert.equal(clampRatio(0.95), 1 - SPLIT_MIN_RATIO);
  assert.equal(clampRatio(-1), SPLIT_MIN_RATIO);
  assert.equal(clampRatio(2), 1 - SPLIT_MIN_RATIO);
});

test("clampRatio maps degenerate input to the center, never NaN", () => {
  // Degenerate math (NaN, infinities) centers the divider rather
  // than slamming it into a pane edge — the drag never sees these,
  // and the fallback should be the least surprising position.
  assert.equal(clampRatio(Number.NaN), 0.5);
  assert.equal(clampRatio(Number.POSITIVE_INFINITY), 0.5);
  assert.equal(clampRatio(Number.NEGATIVE_INFINITY), 0.5);
});

test("snapRatio snaps to halves and thirds within tolerance", () => {
  // Dead-on snap points.
  for (const point of SPLIT_SNAP_POINTS) {
    assert.equal(snapRatio(point), point);
  }
  // Within tolerance (just inside the magnetic zone).
  assert.equal(snapRatio(0.5 + SPLIT_SNAP_TOLERANCE - 0.001), 0.5);
  assert.equal(snapRatio(1 / 3 + 0.02), 1 / 3);
  assert.equal(snapRatio(2 / 3 - 0.02), 2 / 3);
  // Outside tolerance: the raw (clamped) ratio survives.
  const far = 0.45;
  assert.equal(snapRatio(far), far);
});

test("snapRatio still clamps before snapping", () => {
  // 0.1 would snap toward 1/3 only if it were in range — it is not.
  assert.equal(snapRatio(0.1), SPLIT_MIN_RATIO);
  assert.equal(snapRatio(0.31), 1 / 3); // clamped? no: 0.31 is legal and near 1/3
});

test("ratioFromPointer measures the divider track", () => {
  assert.equal(ratioFromPointer(500, rect(0, 1000)), 0.5);
  assert.equal(ratioFromPointer(250, rect(0, 1000)), 0.25);
  // Left-relative, not viewport-relative.
  assert.equal(ratioFromPointer(350, rect(100, 1000)), 0.25);
  // Overshoot clamps to the pane window.
  assert.equal(ratioFromPointer(-50, rect(0, 1000)), SPLIT_MIN_RATIO);
  assert.equal(ratioFromPointer(2000, rect(0, 1000)), 1 - SPLIT_MIN_RATIO);
});

test("ratioFromPointer survives degenerate rects", () => {
  assert.equal(ratioFromPointer(42, rect(0, 0)), 0.5);
  assert.equal(ratioFromPointer(42, rect(10, -5)), 0.5);
});

test("isMirroredSplit recognizes Chrome's copy-the-tab split", () => {
  assert.equal(isMirroredSplit("servers/x/console", "servers/x/console"), true);
  assert.equal(isMirroredSplit("servers/x/console", "servers/x/files"), false);
});
