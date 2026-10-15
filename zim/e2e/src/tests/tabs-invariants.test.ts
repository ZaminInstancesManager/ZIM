/**
 * The tab-is-a-view invariants (TEST_STRATEGY §2, #3/#4/#6 and the
 * generalized E13), driven through the real `zim://app` tab strip:
 * three tabs of one server are three *views* (one process, one
 * start), closing a tab never stops the server, and a tab of a
 * stopped server stays meaningful (state, not error).
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { launchEngine } from "../lib/engine.js";
import {
  adminClient,
  auditWhere,
  createInstance,
  firstNodeId,
  instanceState,
} from "../lib/api.js";
import { launchBrowser, newContext, loginViaUi, seedSession, uiGoto } from "../lib/browser.js";
import { detectTier, skipWithoutDocker } from "../lib/tier.js";
import { run, uniqueName, waitFor } from "../lib/util.js";

test("tabs: three tabs of one server are three views, not three processes (I6)", async (t) => {
  const tier = await detectTier();
  if (skipWithoutDocker(t, tier)) {
    return;
  }
  const engine = await launchEngine({});
  const browser = await launchBrowser();
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("threetabs"), { nodeId });
    await client.post(`/api/v1/instances/${instance.id}/start`);
    await waitFor(
      "running state",
      async () => {
        const state = await instanceState(client, instance.id);
        return state.status === "running" ? state : undefined;
      },
      120_000,
      1_000,
    );

    // Seed a three-tab session — all three pointing at the SAME
    // server console — then load the app chrome.
    const context = await newContext(browser);
    const page = await context.newPage();
    await loginViaUi(page, engine);
    await seedSession(context, engine, {
      v: 2,
      tabs: [
        { path: `servers/${instance.id}/console` },
        { path: `servers/${instance.id}/console` },
        { path: `servers/${instance.id}/console` },
      ],
      active: 0,
    });
    await uiGoto(page, engine, "app");

    // Three tabs are open, exactly ONE container exists.
    await page
      .locator("[data-tab-id]")
      .nth(2)
      .waitFor({ timeout: 30_000 });
    assert.equal(await page.locator("[data-tab-id]").count(), 3);

    const containers = await containerCount(instance.id);
    assert.equal(containers, 1, "three tabs must not mean three processes");

    // And only ONE start ever hit the audit log (the API's own).
    const starts = await auditWhere(client, ["instance.power.start"]);
    assert.equal(starts.length, 1);

    await context.close();
  } finally {
    await browser.close();
    await engine.stop();
  }
});

test("tabs: closing a tab keeps the server running (I4)", async (t) => {
  const tier = await detectTier();
  if (skipWithoutDocker(t, tier)) {
    return;
  }
  const engine = await launchEngine({});
  const browser = await launchBrowser();
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("closetab"), { nodeId });
    await client.post(`/api/v1/instances/${instance.id}/start`);
    await waitFor(
      "running state",
      async () => {
        const state = await instanceState(client, instance.id);
        return state.status === "running" ? state : undefined;
      },
      120_000,
      1_000,
    );

    const context = await newContext(browser);
    const page = await context.newPage();
    await loginViaUi(page, engine);
    await seedSession(context, engine, {
      v: 2,
      tabs: [
        { path: `servers/${instance.id}/console` },
        { path: `servers/${instance.id}/console` },
      ],
      active: 0,
    });
    await uiGoto(page, engine, "app");
    await page
      .locator("[data-tab-id]")
      .nth(1)
      .waitFor({ timeout: 30_000 });

    // Close the active (first) tab via its strip close button.
    await page.locator("[data-tab-id]").first().locator(".tab-close").click();
    await waitFor(
      "one tab left",
      async () => {
        const count = await page.locator("[data-tab-id]").count();
        return count === 1 ? count : undefined;
      },
      15_000,
    );

    // The server is untouched: still running, one container, and
    // no stop ever hit the audit log.
    await sleep(1_500); // Let any would-be stop event land.
    const state = await instanceState(client, instance.id);
    assert.equal(state.status, "running");
    assert.equal(await containerCount(instance.id), 1);
    const stops = await auditWhere(client, ["instance.power.stop", "instance.power.kill"]);
    assert.equal(stops.length, 0, "closing a tab must not stop the server");

    await context.close();
  } finally {
    await browser.close();
    await engine.stop();
  }
});

test("tabs: a tab of a never-started server issues no lifecycle and stays meaningful (I3, E13)", async () => {
  // Tier-independent: the instance is never started, so this holds
  // identically with Docker and in degraded (null) mode.
  const engine = await launchEngine({});
  const browser = await launchBrowser();
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("neverrun"), { nodeId });
    // Deliberately NEVER started.

    const context = await newContext(browser);
    const page = await context.newPage();
    await loginViaUi(page, engine);
    await uiGoto(page, engine, `servers/${instance.id}/console`);

    // The console page renders the stopped state — the Start button
    // is enabled, the status row says not-running — not an error.
    await page.locator("#btn-start").waitFor({ timeout: 30_000 });
    const stateText = await page.locator("#state-label").textContent();
    assert.ok(stateText !== null);
    assert.doesNotMatch(stateText, /error|unknown/);
    const startEnabled = await page.locator("#btn-start").isDisabled();
    assert.equal(startEnabled, false);

    // The tab's existence issued no lifecycle command at all.
    const power = await auditWhere(client, [
      "instance.power.start",
      "instance.power.stop",
      "instance.power.restart",
      "instance.power.kill",
    ]);
    assert.equal(power.length, 0, "a tab is a view, not a lifecycle command");

    await context.close();
  } finally {
    await browser.close();
    await engine.stop();
  }
});

/** Count live containers carrying one instance's id label. */
async function containerCount(instanceId: string): Promise<number> {
  const result = await run(
    "docker",
    ["ps", "--filter", `label=ZIM.InstanceId=${instanceId}`, "-q"],
    { timeoutMs: 30_000 },
  );
  return result.stdout.split("\n").filter((line) => line.trim() !== "").length;
}

/** A small local sleep (kept private — util's sleep re-exports it). */
function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
