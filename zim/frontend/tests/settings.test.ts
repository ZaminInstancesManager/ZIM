/**
 * The settings-form pure model (M29): the MiB ↔ bytes conversions
 * and the form-to-PATCH builder — the §8 gate's client half (the
 * builder refuses bad shapes before the round-trip; the engine
 * stays the authority).
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { bytesToMib, mibToBytes, settingsFormToPatch } from "../src/api.js";
import type { SettingsFormValues } from "../src/api.js";

test("MiB conversions round-trip the panel convention", () => {
  assert.equal(mibToBytes(512), 512 * 1024 * 1024);
  assert.equal(bytesToMib(512 * 1024 * 1024), 512);
  assert.equal(mibToBytes(0), 0);
  // Fractional MiB survive honestly (512.5 MiB is a real quantity).
  assert.equal(mibToBytes(512.5), Math.round(512.5 * 1024 * 1024));
  assert.ok(Math.abs(bytesToMib(mibToBytes(512.5)) - 512.5) < 1e-9);
});

/** A well-formed form's baseline values. */
function form(overrides: Partial<SettingsFormValues> = {}): SettingsFormValues {
  return {
    name: "survival",
    description: "the survival world",
    startup: "java -jar server.jar",
    image: "eclipse-temurin:21",
    externalId: "",
    memoryMib: "2048",
    swapMib: "0",
    diskMib: "10240",
    cpuMillicores: "1000",
    ioWeight: "500",
    threads: "",
    backupLimit: "10",
    databaseLimit: "",
    allocationLimit: "",
    crashDetectionEnabled: true,
    skipEggScripts: false,
    oomDisabled: false,
    ...overrides,
  };
}

test("the builder sends the full editable form", () => {
  const request = settingsFormToPatch(form());
  assert.ok(request !== null);
  assert.equal(request.name, "survival");
  assert.equal(request.description, "the survival world");
  assert.equal(request.startup, "java -jar server.jar");
  assert.equal(request.image, "eclipse-temurin:21");
  assert.deepEqual(request.limits, {
    memory: 2048 * 1024 * 1024,
    swap: 0,
    disk: 10240 * 1024 * 1024,
    io_weight: 500,
    cpu_millicores: 1000,
    threads: null,
    oom_disabled: false,
  });
  assert.equal(request.backup_limit, 10);
  assert.equal(request.database_limit, undefined);
  assert.equal(request.allocation_limit, undefined);
  assert.equal(request.external_id, undefined);
  assert.equal(request.crash_detection_enabled, true);
  assert.equal(request.skip_egg_scripts, false);
});

test("the builder refuses the shape-invalid", () => {
  // Empty name: the engine's own row invariant.
  assert.equal(settingsFormToPatch(form({ name: "   " })), null);
  // Non-numeric and negative numbers refuse before the round-trip.
  assert.equal(settingsFormToPatch(form({ memoryMib: "lots" })), null);
  assert.equal(settingsFormToPatch(form({ cpuMillicores: "-1" })), null);
  assert.equal(settingsFormToPatch(form({ diskMib: "" })), null);
  // A filled budget that is not a whole number refuses too.
  assert.equal(settingsFormToPatch(form({ backupLimit: "1.5" })), null);
});

test("the builder carries the optional fields honestly", () => {
  const request = settingsFormToPatch(
    form({
      externalId: "legacy-42",
      databaseLimit: "5",
      allocationLimit: "3",
      threads: "0-3,8",
      oomDisabled: true,
      skipEggScripts: true,
    }),
  );
  assert.ok(request !== null);
  assert.equal(request.external_id, "legacy-42");
  assert.equal(request.database_limit, 5);
  assert.equal(request.allocation_limit, 3);
  assert.equal(request.limits?.threads, "0-3,8");
  assert.equal(request.limits?.oom_disabled, true);
  assert.equal(request.skip_egg_scripts, true);
});
