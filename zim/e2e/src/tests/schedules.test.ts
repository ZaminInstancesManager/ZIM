/**
 * The schedules scenario (§1.4): create a cron schedule on an
 * instance, list it, verify the validation contract (a bad cron
 * expression is refused), update it, trigger it on demand, and
 * delete it. Durable-state operations are tier-independent; the
 * run-now trigger forks on the tier (AD-30): with a container
 * runtime the dispatch creates a durable job, without one the
 * engine refuses it up front with 400 `runtime unavailable` —
 * both are the contract, each on its own tier.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { launchEngine } from "../lib/engine.js";
import { adminClient, createInstance, firstNodeId } from "../lib/api.js";
import { detectTier } from "../lib/tier.js";
import { uniqueName } from "../lib/util.js";

test("schedules: create → validate → list → update → run-now → delete", async () => {
  const engine = await launchEngine({});
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("sched"), { nodeId });

    // Create: a 5-field cron with one power action (the domain's
    // internally-tagged enum: {"type": "power", "action": …}).
    // only_when_online stays off: run-now on a stopped instance is
    // part of this scenario's assertions.
    const created = await client.post(`/api/v1/instances/${instance.id}/schedules`, {
      name: "nightly restart",
      cron: "0 4 * * *",
      is_active: true,
      only_when_online: false,
      actions: [{ type: "power", action: "restart" }],
    });
    const scheduleId = String(created.id);
    assert.ok(scheduleId.length > 0);

    // Validation: a 4-field cron expression is refused.
    let refused = false;
    try {
      await client.post(`/api/v1/instances/${instance.id}/schedules`, {
        name: "broken",
        cron: "0 4 * *",
        actions: [],
      });
    } catch (error) {
      refused = (error as { status: number }).status === 400;
    }
    assert.ok(refused, "a malformed cron expression must be a 400");

    // List: exactly the one schedule, carrying its cron.
    const list = await client.get(`/api/v1/instances/${instance.id}/schedules`);
    const rows = list.data as Array<Record<string, unknown>>;
    assert.equal(rows.length, 1);
    assert.equal(String(rows[0].cron), "0 4 * * *");
    assert.equal(rows[0].is_active, true);

    // Update: rename + pause it.
    const updated = await client.patch(
      `/api/v1/instances/${instance.id}/schedules/${scheduleId}`,
      { name: "nightly restart (paused)", is_active: false },
    );
    assert.equal(String(updated.name), "nightly restart (paused)");
    assert.equal(updated.is_active, false);

    // Trigger now: on the docker tier a durable job is created; on
    // the null tier the engine refuses the dispatch up front — the
    // power action needs a container runtime that is not there —
    // and that 400 refusal IS the null tier's honest contract.
    const tier = await detectTier();
    if (tier.tier === "docker") {
      const triggered = await client.post(
        `/api/v1/instances/${instance.id}/schedules/${scheduleId}/run`,
      );
      assert.ok(triggered !== null && triggered !== undefined, "run-now answered");
    } else {
      let runRefused = false;
      try {
        await client.post(`/api/v1/instances/${instance.id}/schedules/${scheduleId}/run`);
      } catch (error) {
        runRefused = (error as { status: number }).status === 400;
      }
      assert.ok(
        runRefused,
        `run-now on the null tier must be refused with 400 (${tier.detail})`,
      );
    }

    // Delete: the list is empty again.
    await client.del(`/api/v1/instances/${instance.id}/schedules/${scheduleId}`);
    const after = await client.get(`/api/v1/instances/${instance.id}/schedules`);
    assert.equal((after.data as unknown[]).length, 0);
  } finally {
    await engine.stop();
  }
});
