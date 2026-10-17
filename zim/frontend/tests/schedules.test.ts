/**
 * The schedule-family pure helpers (M29): the structural cron
 * pre-check mirrored from the engine's domain rule (5
 * whitespace-separated non-empty fields — the engine remains the
 * authority; this is the form's preview).
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { isStructurallyValidCron } from "../src/api.js";

test("isStructurallyValidCron accepts real 5-field expressions", () => {
  assert.equal(isStructurallyValidCron("0 4 * * *"), true);
  assert.equal(isStructurallyValidCron("*/5 * * * *"), true);
  assert.equal(isStructurallyValidCron("30 2 1 1 *"), true);
  assert.equal(isStructurallyValidCron("0 0 * * 0"), true);
});

test("isStructurallyValidCron refuses the truncated and the empty", () => {
  // The copy-paste truncations the domain layer catches.
  assert.equal(isStructurallyValidCron("0 4 * *"), false);
  assert.equal(isStructurallyValidCron("0 4 * * * *"), false);
  assert.equal(isStructurallyValidCron(""), false);
  assert.equal(isStructurallyValidCron("* * * *"), false);
});

test("isStructurallyValidCron tolerates surrounding and inner runs of spaces", () => {
  // split on /\s+/ collapses runs; trim handles the edges — the
  // form may paste "  0  4  *  *  *  " and still pass.
  assert.equal(isStructurallyValidCron("  0 4 * * *  "), true);
  assert.equal(isStructurallyValidCron("0  4  *   *  *"), true);
  // Trailing runs trim away first — what remains is the honest
  // 4-field truncation, refused.
  assert.equal(isStructurallyValidCron("0 4 * *  "), false);
});
