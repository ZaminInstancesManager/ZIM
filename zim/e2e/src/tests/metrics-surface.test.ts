/**
 * The metrics surface (M30): the last declared page graduating to
 * its own real surface, driven through the real product UI in the
 * real browser, over the real engine.
 *
 * Tier-honest by design (AD-30): the history family itself is a
 * durable-store read — the window switcher, the endpoint's answer
 * shape, and the honest empty state run identically on every tier
 * (Linux, macOS, Windows). The live-sample walk needs a running
 * workload, so it rides the docker tier alone: a real busybox
 * container, a real monitor persisting at a fast cadence, real
 * charts rendering the derived series.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { launchEngine } from "../lib/engine.js";
import { adminClient, createInstance, firstNodeId, instanceState } from "../lib/api.js";
import { launchBrowser, loginViaUi, newContext, uiGoto } from "../lib/browser.js";
import { detectTier, skipWithoutDocker } from "../lib/tier.js";
import { uniqueName, waitFor } from "../lib/util.js";

/** Poll a probe (returning undefined = not yet) with a CI-generous timeout. */
async function eventually<T>(what: string, probe: () => Promise<T | undefined>): Promise<T> {
  return await waitFor(what, probe, 120_000, 1_000);
}

test("metrics: the window switcher and the honest empty state run on every tier", async () => {
  const engine = await launchEngine({});
  const browser = await launchBrowser();
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("metrics-empty"), { nodeId });

    // The endpoint answers with the documented envelope — honestly
    // empty for an instance that never ran.
    const history = (await client.get(
      `/api/v1/instances/${instance.id}/metrics/history`,
    )) as Record<string, unknown>;
    assert.equal(history.instance_id, instance.id);
    assert.equal(history.window, "1h");
    assert.equal(history.resolution_seconds, 20);
    assert.deepEqual(history.points, []);
    const summary = history.summary as Record<string, unknown>;
    assert.equal(summary.point_count, 0);
    assert.equal(summary.first_ts, null);

    // The window vocabulary round-trips.
    for (const [window, resolution] of [
      ["15m", 5],
      ["6h", 120],
      ["24h", 480],
      ["7d", 3360],
    ] as const) {
      const answer = (await client.get(
        `/api/v1/instances/${instance.id}/metrics/history?window=${window}`,
      )) as Record<string, unknown>;
      assert.equal(answer.window, window);
      assert.equal(answer.resolution_seconds, resolution);
    }
    // An unknown window is the structured refusal naming the vocabulary.
    const refused = await client.tryGet(
      `/api/v1/instances/${instance.id}/metrics/history?window=fortnight`,
    );
    assert.equal(refused.status, 400);
    assert.match(refused.body, /15m, 1h, 6h, 24h, 7d/);

    // The page: the window switcher is real and the empty state says
    // exactly what it is — no fake flat lines.
    const context = await newContext(browser);
    const page = await context.newPage();
    await loginViaUi(page, engine);
    await uiGoto(page, engine, `servers/${instance.id}/metrics`);
    await page.locator("#window-bar").waitFor({ state: "visible", timeout: 15_000 });
    const pills = page.locator("#window-bar .window-pill");
    assert.equal(await pills.count(), 5);
    assert.equal(await pills.first().innerText(), "15m");

    // Switching windows re-queries (the pressed pill moves and the
    // status line names the window).
    await page.locator('.window-pill[data-window="24h"]').click();
    await page
      .locator("#metrics-message")
      .filter({ hasText: /24h/ })
      .waitFor({ state: "visible", timeout: 15_000 });
    assert.equal(
      await page.locator('.window-pill[data-window="24h"]').getAttribute("aria-pressed"),
      "true",
    );
    assert.equal(
      await page.locator('.window-pill[data-window="1h"]').getAttribute("aria-pressed"),
      "false",
    );

    // The honest empty state renders.
    await page.locator("#metrics-empty").waitFor({ state: "visible", timeout: 15_000 });
    await context.close();
  } finally {
    await browser.close();
    await engine.stop();
  }
});

test("metrics: the running workload's series fills and the charts render it (docker tier)", async (t) => {
  const tier = await detectTier();
  if (skipWithoutDocker(t, tier)) {
    return;
  }
  // A fast persist cadence so the durable series fills in test time
  // (the default 10 s would make this walk wait a minute for three
  // points).
  const engine = await launchEngine({ env: { ZIM_METRICS_PERSIST_SECS: "2" } });
  const browser = await launchBrowser();
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("metrics-live"), { nodeId });

    // Start the real container; the monitor samples and persists.
    await client.post(`/api/v1/instances/${instance.id}/start`);
    await eventually("running state", async () => {
      const state = await instanceState(client, instance.id);
      return state.status === "running" ? state : undefined;
    });

    // The durable series fills: several persisted points with the
    // derived shape (memory gauges ride every point).
    await eventually("persisted series", async () => {
      const history = (await client.get(
        `/api/v1/instances/${instance.id}/metrics/history`,
      )) as Record<string, unknown>;
      const points = history.points as Array<Record<string, unknown>>;
      if (points.length < 3) {
        return undefined;
      }
      for (const point of points) {
        // busybox reports memory; the gauges must be non-null.
        assert.ok(
          typeof point.memory_usage_bytes === "number" && point.memory_usage_bytes > 0,
          "the memory gauge rides every point",
        );
      }
      return history;
    });

    // The page renders the series: the memory canvas carries its
    // known-point count; the summary line narrates the window.
    const context = await newContext(browser);
    const page = await context.newPage();
    await loginViaUi(page, engine);
    await uiGoto(page, engine, `servers/${instance.id}/metrics`);
    await eventually("charts render the series", async () => {
      const known = await page
        .locator('#chart-memory[data-series="memory"]')
        .getAttribute("data-points");
      const count = Number(known ?? "0");
      return count >= 3 ? count : undefined;
    });
    await page
      .locator("#summary-line")
      .filter({ hasText: /point\(s\) at 20s resolution/ })
      .waitFor({ state: "visible", timeout: 15_000 });

    // The live strip wakes up: the §11 stream's resource-usage
    // samples turn the placeholder pills into live gauges.
    await eventually("live memory gauge", async () => {
      const text = await page.locator("#live-memory").innerText();
      return text.startsWith("memory ") && !text.endsWith("—") ? text : undefined;
    });

    // Window switch on a live series: the 15m view re-renders at its
    // own resolution.
    await page.locator('.window-pill[data-window="15m"]').click();
    await page
      .locator("#summary-line")
      .filter({ hasText: /5s resolution/ })
      .waitFor({ state: "visible", timeout: 15_000 });

    // Stop the container (the engine outlives the walk).
    await client.post(`/api/v1/instances/${instance.id}/stop`);
    await eventually("offline state", async () => {
      const state = await instanceState(client, instance.id);
      return state.status === "offline" ? state : undefined;
    });
    await context.close();
  } finally {
    await browser.close();
    await engine.stop();
  }
});
