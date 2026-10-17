/**
 * The backup-family pure helpers (M29): byte formatting for the
 * honest sizes, and the job-state terminal vocabulary the
 * create/restore follow loop polls against.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { formatBytes, jobIsTerminal } from "../src/api.js";

test("formatBytes renders the file manager's convention", () => {
  assert.equal(formatBytes(0), "0 B");
  assert.equal(formatBytes(512), "512 B");
  assert.equal(formatBytes(1024), "1.0 KiB");
  assert.equal(formatBytes(1536), "1.5 KiB");
  assert.equal(formatBytes(1024 * 1024), "1.0 MiB");
  assert.equal(formatBytes(5 * 1024 * 1024 * 1024), "5.0 GiB");
  // The rounding switch: >= 100 drops the fraction.
  assert.equal(formatBytes(100 * 1024), "100 KiB");
  assert.equal(formatBytes(1000 * 1024), "1000 KiB");
  assert.equal(formatBytes(99512), "97.2 KiB");
});

test("jobIsTerminal knows the engine's JobState vocabulary", () => {
  // Terminal states: the follow loop stops and reports.
  assert.equal(jobIsTerminal("succeeded"), true);
  assert.equal(jobIsTerminal("failed"), true);
  assert.equal(jobIsTerminal("cancelled"), true);
  // Still moving: the poll keeps asking.
  assert.equal(jobIsTerminal("pending"), false);
  assert.equal(jobIsTerminal("in_progress"), false);
  assert.equal(jobIsTerminal("cleanup_pending"), false);
  // Unknown vocabulary is not terminal (never lie about progress).
  assert.equal(jobIsTerminal(""), false);
  assert.equal(jobIsTerminal("done"), false);
  assert.equal(jobIsTerminal("Succeeded"), false);
});
