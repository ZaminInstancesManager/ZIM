/**
 * The console scenario (§1.4) at the browser level: the per-instance
 * console page replays the buffered output on load, then keeps
 * appending live lines over the `zim-events.v1` stream — the exact
 * contract a tab pane relies on. Docker tier: the busybox ticker
 * must genuinely be producing output.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { launchEngine } from "../lib/engine.js";
import {
  adminClient,
  createInstance,
  firstNodeId,
  instanceState,
} from "../lib/api.js";
import { launchBrowser, newContext, loginViaUi, uiGoto } from "../lib/browser.js";
import { detectTier, skipWithoutDocker } from "../lib/tier.js";
import { uniqueName, waitFor } from "../lib/util.js";

test("console: the server console page replays the buffer and streams live ticks", async (t) => {
  const tier = await detectTier();
  if (skipWithoutDocker(t, tier)) {
    return;
  }
  const engine = await launchEngine({});
  const browser = await launchBrowser();
  // Hoisted: the finally must close the context even when the test
  // fails before its own cleanup line (a leaked context holds the
  // browser open — the slot-jam lesson, browser half).
  let context: Awaited<ReturnType<typeof newContext>> | undefined;
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("console"), { nodeId });
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

    // Log in through the UI, then open the console surface.
    context = await newContext(browser);
    const page = await context.newPage();
    await loginViaUi(page, engine);
    await uiGoto(page, engine, `servers/${instance.id}/console`);

    // The page's status row reflects the observed state.
    await page
      .locator("#state-label")
      .filter({ hasText: /running/ })
      .waitFor({ timeout: 30_000 });

    // The buffered replay lands in the console view (shadow DOM —
    // Playwright's text matching pierces open shadow roots).
    await page
      .getByText("zim-e2e-tick")
      .first()
      .waitFor({ timeout: 30_000 });

    // The live stream keeps appending: count grows while we watch.
    const countAt = async (): Promise<number> =>
      await page.locator("#console").evaluate((element) => {
        const root = element.shadowRoot;
        return root?.textContent?.split("zim-e2e-tick").length ?? 1;
      });
    const early = await countAt();
    await waitFor(
      "live console growth",
      async () => {
        const now = await countAt();
        return now > early ? now : undefined;
      },
      30_000,
      1_000,
    );

    // The input control is present (§10.3 console input surface).
    assert.ok(await page.locator("#input-line").count() > 0);

    await context.close();
  } finally {
    // Order matters: the context first (its page may hold a live
    // stream socket), then the browser, then the engine — every
    // step failing soft so a wedged step can never wedge the
    // file's process.
    await context?.close().catch(() => {});
    await browser.close();
    await engine.stop();
  }
});
