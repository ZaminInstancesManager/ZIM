/**
 * The core §1.4 scenario — "create an instance, start it, view its
 * console, stop it" — against the real Docker runtime when the
 * tier provides one, and the degraded-mode contract when it
 * doesn't (the engine boots API-only with the null runtime: starts
 * are refused with `runtime_unavailable`, never queued as fake
 * success).
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { launchEngine } from "../lib/engine.js";
import {
  adminClient,
  ApiError,
  consoleLines,
  createInstance,
  firstNodeId,
  instanceState,
} from "../lib/api.js";
import { detectTier, skipWithoutDocker } from "../lib/tier.js";
import { uniqueName } from "../lib/util.js";

test("lifecycle: create → start → console ticks → stop (docker tier)", async (t) => {
  const tier = await detectTier();
  if (skipWithoutDocker(t, tier)) {
    return;
  }
  const engine = await launchEngine({});
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("lifecycle"), { nodeId });

    // Created instances start in a non-running state.
    const before = await instanceState(client, instance.id);
    assert.notEqual(before.status, "running");

    // Start: 2xx, then the observed state converges to running.
    const started = await client.post(`/api/v1/instances/${instance.id}/start`);
    assert.ok(started, "start answered");
    await eventually("running state", async () => {
      const state = await instanceState(client, instance.id);
      return state.status === "running" ? state : undefined;
    });

    // The container genuinely ticks: the console buffer collects lines.
    await eventually("console output", async () => {
      const lines = await consoleLines(client, instance.id);
      return lines.some((line) => line.includes("zim-e2e-tick")) ? lines : undefined;
    });

    // Stop: graceful — SIGTERM'd container, state converges to stopped.
    await client.post(`/api/v1/instances/${instance.id}/stop`);
    await eventually("stopped state", async () => {
      const state = await instanceState(client, instance.id);
      return state.status === "stopped" ? state : undefined;
    });

    // And restart still works after a stop (the loop is real).
    await client.post(`/api/v1/instances/${instance.id}/start`);
    await eventually("running again", async () => {
      const state = await instanceState(client, instance.id);
      return state.status === "running" ? state : undefined;
    });
    await client.post(`/api/v1/instances/${instance.id}/stop`);
    await eventually("stopped again", async () => {
      const state = await instanceState(client, instance.id);
      return state.status === "stopped" ? state : undefined;
    });
  } finally {
    await engine.stop();
  }
});

test("lifecycle: without Docker the engine refuses starts honestly (null tier)", async (t) => {
  const tier = await detectTier();
  if (tier.tier === "docker") {
    t.skip("null-tier contract only — this host has Docker");
    return;
  }
  const engine = await launchEngine({});
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("degraded"), { nodeId });

    // Degraded mode is a 503 with the runtime_unavailable code —
    // never a fake "started" (invariant 2's spirit at boot scope).
    let refused: ApiError | undefined;
    try {
      await client.post(`/api/v1/instances/${instance.id}/start`);
    } catch (error) {
      refused = error as ApiError;
    }
    assert.ok(refused, "start must be refused in degraded mode");
    assert.equal(refused.status, 503);
    assert.match(refused.body, /runtime/i);

    // Instance creation itself still works in degraded mode.
    const listed = await client.get("/api/v1/instances");
    assert.ok(
      (listed.data as Array<Record<string, unknown>>).some((row) => String(row.id) === instance.id),
      "created instance is listed",
    );
  } finally {
    await engine.stop();
  }
});

/** Poll a probe (returning undefined = not yet) with a CI-generous timeout. */
async function eventually<T>(
  what: string,
  probe: () => Promise<T | undefined>,
): Promise<T> {
  const { waitFor } = await import("../lib/util.js");
  return await waitFor(what, probe, 120_000, 1_000);
}
