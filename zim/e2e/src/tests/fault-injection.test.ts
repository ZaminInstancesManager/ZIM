/**
 * Fault injection (TEST_STRATEGY §3):
 *
 * 1. Engine crash (SIGKILL) mid-flight → relaunch on the same data
 *    dir → reconciliation: the persisted "running" flag is never
 *    trusted (invariant 1); a genuinely-running container is
 *    re-adopted, the API is honest again, and the crash left the
 *    discovery file behind (stale-file-as-hint semantics).
 * 2. Renderer crash → the engine is unaffected (health, API, and a
 *    second live page keep working).
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { join } from "node:path";
import { launchEngine } from "../lib/engine.js";
import { adminClient, createInstance, firstNodeId, instanceState } from "../lib/api.js";
import { launchBrowser, newContext, loginViaUi, uiGoto } from "../lib/browser.js";
import { detectTier, skipWithoutDocker } from "../lib/tier.js";
import { uniqueName, waitFor } from "../lib/util.js";

test("fault injection: engine SIGKILL → relaunch → running container is re-adopted, not duplicated", async (t) => {
  const tier = await detectTier();
  if (skipWithoutDocker(t, tier)) {
    return;
  }
  const engine = await launchEngine({});
  let crashed = false;
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("crash"), { nodeId });
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

    // ── The fault: the engine dies without cleanup.
    await engine.crash();
    crashed = true;
    // The discovery file survived the crash (it is a hint, not a
    // lock — the shell's probe decides, not the file's presence).
    assert.equal(existsSync(join(engine.dataDir, "engine.json")), true);

    // ── Relaunch on the SAME data dir (a fresh port).
    const relaunched = await launchEngine({ dataDir: engine.dataDir });
    try {
      // The API is honest again, and the persisted state was never
      // trusted: reconciliation re-observed the still-running
      // container and adopted it (invariants 1 + 10).
      const client2 = await adminClient(relaunched);
      await waitFor(
        "re-adopted running state",
        async () => {
          const state = await instanceState(client2, instance.id);
          return state.status === "running" ? state : undefined;
        },
        120_000,
        1_000,
      );

      // The discovery file now names the NEW engine.
      const discovery = JSON.parse(
        await import("node:fs/promises").then((fs) =>
          fs.readFile(join(engine.dataDir, "engine.json"), "utf8"),
        ),
      ) as { port: number };
      assert.equal(discovery.port, relaunched.port);
    } finally {
      await relaunched.stop();
    }
  } finally {
    if (!crashed) {
      await engine.stop();
    }
  }
});

test("fault injection: a renderer crash leaves the engine unaffected", async () => {
  const engine = await launchEngine({});
  const browser = await launchBrowser();
  try {
    const client = await adminClient(engine);
    const context = await newContext(browser);
    const healthy = await context.newPage();
    await loginViaUi(healthy, engine);
    await uiGoto(healthy, engine, "newtab");

    // ── The fault: the second page's renderer dies.
    const doomed = await context.newPage();
    await uiGoto(doomed, engine, "newtab");
    await doomed.goto("chrome://crash").catch(() => {
      // The renderer died mid-navigation; the exact Playwright
      // error shape is not the assertion's business.
    });

    // ── The engine is unaffected: health answers, the API serves,
    //    and the healthy page still talks to the origin.
    const health = await client.get("/api/v1/health");
    assert.equal(health.status, "ok");
    await healthy.locator("zim-login-form .who").waitFor({ timeout: 30_000 });
    await uiGoto(healthy, engine, "servers");
    await healthy.waitForLoadState("domcontentloaded");

    await context.close();
  } finally {
    await browser.close();
    await engine.stop();
  }
});
