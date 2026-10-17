/**
 * Invariant I5: restoring a session after a browser restart
 * re-opens *views* and issues no lifecycle commands. The persisted
 * `zim.session.v2` snapshot survives a real browser relaunch
 * (persistent profile), and the audit log is the arbiter: the only
 * power events in it are the harness's own original start.
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
import { launchBrowser, loginViaUi, newPersistentContext, seedSession, uiGoto } from "../lib/browser.js";
import { detectTier, skipWithoutDocker } from "../lib/tier.js";
import { tempDir, uniqueName, waitFor } from "../lib/util.js";

test("session: a browser restart restores the tab set without touching the server (I5)", async (t) => {
  const tier = await detectTier();
  if (skipWithoutDocker(t, tier)) {
    return;
  }
  const engine = await launchEngine({});
  const browser = await launchBrowser(); // for the login helper's throwaway page
  const profile = tempDir("profile");
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("restore"), { nodeId });
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

    // ── Browser session #1: sign in, seed a one-tab session, load it.
    const first = await newPersistentContext(profile);
    {
      const page = await first.newPage();
      await loginViaUi(page, engine);
      await seedSession(first, engine, {
        v: 2,
        tabs: [{ path: `servers/${instance.id}/console` }],
        active: 0,
      });
      await uiGoto(page, engine, "app");
      // `#tabstrip .tab`: pane hosts also carry data-tab-id (the
      // app's own convention) — an unscoped selector counts 2N.
      await page.locator("#tabstrip .tab").first().waitFor({ timeout: 30_000 });
      assert.equal(await page.locator("#tabstrip .tab").count(), 1);
      await first.close(); // the whole "browser" goes away
    }

    // ── Browser session #2: same profile — the session restores.
    const second = await newPersistentContext(profile);
    {
      const page = await second.newPage();
      await uiGoto(page, engine, "app");
      await page.locator("#tabstrip .tab").first().waitFor({ timeout: 30_000 });
      assert.equal(await page.locator("#tabstrip .tab").count(), 1);
      // The restored tab really re-opened the server console pane
      // (same-origin iframe; frameLocator reaches into it).
      const pane = page.frameLocator('iframe.pane-frame').locator("#server-name");
      await pane.first().waitFor({ timeout: 30_000 });
      await second.close();
    }

    // ── The arbiter: exactly one start (the harness's own), no stops.
    const starts = await auditWhere(client, ["instance.power.start"]);
    const stops = await auditWhere(client, ["instance.power.stop", "instance.power.kill"]);
    assert.equal(starts.length, 1, "restoring must not issue starts");
    assert.equal(stops.length, 0, "restoring must not issue stops");
    const state = await instanceState(client, instance.id);
    assert.equal(state.status, "running");
  } finally {
    await browser.close();
    await engine.stop();
  }
});
