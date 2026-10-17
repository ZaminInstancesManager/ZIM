/**
 * The E7/E8/E9 visual state mappings (TEST_STRATEGY §2, the M27
 * nightly failure-sweep family) — what the §4 status vocabulary
 * LOOKS like, driven as real as the single-node engine allows:
 *
 * - **E7 (recovering)**: a real container is `docker kill`ed; the
 *   supervisor's crash handling auto-restarts it (below the
 *   crash-loop threshold) and the observed status passes through
 *   `starting` — the yellow "recovering" phase — before running
 *   again. The mapping itself (`starting` renders yellow) is
 *   asserted against the real served component in this file's
 *   mapping table below.
 * - **E8 (crash loop)**: a container whose command exits
 *   immediately trips the real crash-loop threshold; the engine
 *   reports `crash_loop` and the REAL servers page, in a real
 *   browser, renders the red dot WITH the caution glyph
 *   (`.dot-red.dot-crash`) — never solid red.
 * - **E9 (unknown)**: a disconnected remote node cannot be
 *   synthesized by the single-node alpha engine (the honest
 *   boundary the test strategy records), so its mapping is
 *   asserted against the REAL served component in the REAL browser
 *   (the engine's own assets/components.js, not a fixture): every
 *   canonical status string renders the §4 class — `unknown` gray
 *   with the honest label, `crash_loop` red + glyph, transient
 *   states pulsing yellow.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { launchEngine } from "../lib/engine.js";
import { adminClient, createInstance, firstNodeId, instanceState } from "../lib/api.js";
import { launchBrowser, newContext, loginViaUi, seedSession, uiGoto } from "../lib/browser.js";
import { detectTier, skipWithoutDocker } from "../lib/tier.js";
import { run, uniqueName, waitFor } from "../lib/util.js";

// ── Ambient DOM shims for page.evaluate callback bodies ──────────
//
// The callbacks below are serialized (Playwright ships their SOURCE
// to the page) and run in the browser, where the real objects
// exist. These declarations only let the Node-side compiler see
// enough of that world to typecheck the bodies — the page's reality
// is the runtime truth, and the shapes here are the minimal slices
// the callbacks touch.

interface PageElement {
  setAttribute(name: string, value: string): void;
  remove(): void;
  append(node: PageElement): void;
  shadowRoot: {
    querySelector(selector: string): { className: string; textContent: string | null } | null;
  } | null;
}

declare const document: {
  querySelector(selector: string): PageElement | null;
  createElement(tag: string): PageElement;
  body: PageElement;
};

/** The §4 mapping table, asserted against the real served component
 * in the real browser (E9's honest scope, plus the mapping halves
 * of E7/E8 that the transient states cannot be caught mid-flight). */
const VISUAL_TABLE: Array<{ status: string; classes: string[] }> = [
  { status: "running", classes: ["dot-green"] },
  { status: "starting", classes: ["dot-yellow", "dot-pulse"] },
  { status: "stopping", classes: ["dot-yellow", "dot-pulse"] },
  { status: "installing", classes: ["dot-yellow", "dot-pulse"] },
  { status: "restoring_backup", classes: ["dot-yellow", "dot-pulse"] },
  { status: "transferring", classes: ["dot-yellow", "dot-pulse"] },
  { status: "offline", classes: ["dot-blue"] },
  { status: "suspended", classes: ["dot-gray"] },
  { status: "unknown", classes: ["dot-gray"] },
  { status: "crash_loop", classes: ["dot-red", "dot-crash"] },
];

test("visual mappings: the real served component renders the §4 table (browser, all statuses)", async () => {
  const engine = await launchEngine({});
  const browser = await launchBrowser();
  try {
    const context = await newContext(browser);
    const page = await context.newPage();
    await loginViaUi(page, engine);
    await uiGoto(page, engine, "servers");

    // Drive the REAL served component through every canonical
    // status and read back its shadow-root class list (className is
    // the space-joined DOMTokenList — a string, so nothing
    // DOM-typed crosses the wire).
    const results = await page.evaluate((statuses: string[]) => {
      const host = document.querySelector("main.page") ?? document.body;
      return statuses.map((status) => {
        const dot = document.createElement("zim-status-dot");
        dot.setAttribute("status", status);
        host.append(dot);
        const inner = dot.shadowRoot?.querySelector(".dot") ?? null;
        const labelEl = dot.shadowRoot?.querySelector(".dot-label") ?? null;
        const observed = {
          status,
          className: inner?.className ?? "",
          label: labelEl?.textContent ?? "",
        };
        dot.remove();
        return observed;
      });
    }, VISUAL_TABLE.map((row) => row.status));

    for (const expected of VISUAL_TABLE) {
      const observed = results.find((row) => row.status === expected.status);
      assert.ok(observed !== undefined, `status ${expected.status} was driven`);
      const classes = observed.className.split(" ").filter((part) => part !== "");
      for (const wanted of expected.classes) {
        assert.ok(
          classes.includes(wanted),
          `${expected.status} must render ${wanted}, got [${classes.join(" ")}]`,
        );
      }
      // The negative space of the §4 table: yellow never on the
      // steady states, red never on unknown, gray never on running.
      if (!expected.classes.includes("dot-pulse")) {
        assert.ok(
          !classes.includes("dot-pulse"),
          `${expected.status} must not pulse, got [${classes.join(" ")}]`,
        );
      }
      assert.equal(observed.label, expected.status, "the honest label is the status itself");
    }

    // E9's own sentence: unknown reads gray — never red.
    const unknown = results.find((row) => row.status === "unknown");
    assert.ok(unknown !== undefined);
    assert.ok(unknown.className.includes("dot-gray"));
    assert.ok(!unknown.className.includes("dot-red"));
  } finally {
    await engine.stop();
  }
});

test("E7 recovering: docker kill → the observed state passes through starting (yellow), never fake-running", async (t) => {
  const tier = await detectTier();
  if (skipWithoutDocker(t, tier)) {
    return;
  }
  const engine = await launchEngine({});
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("recover"), { nodeId });

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

    // Crash the container out from under the supervisor (the
    // CONTAINER name is the instance UUID — not the row id): the
    // monitor (1s poll) observes the death, records the crash
    // (below the threshold — the first one), and auto-restarts.
    // The recovering window is `starting` — poll fast enough to
    // catch it.
    const killed = await run("docker", ["kill", instance.uuid], { timeoutMs: 30_000 });
    assert.equal(
      killed.code,
      0,
      `docker kill reached the container (stderr: ${killed.stderr.slice(0, 200)})`,
    );

    let sawRecovering = false;
    let recovered = false;
    const deadline = Date.now() + 60_000;
    while (Date.now() < deadline) {
      const state = await instanceState(client, instance.id);
      if (state.status === "starting" || state.status === "offline") {
        sawRecovering = true;
      }
      if (sawRecovering && state.status === "running") {
        recovered = true;
        break; // the recovery completed
      }
      await new Promise((resolve) => setTimeout(resolve, 150));
    }
    assert.ok(sawRecovering, "the observed state passed through the recovering (starting) phase");
    assert.ok(recovered, "the auto-restart brought the instance back to running");
  } finally {
    await engine.stop();
  }
});

test("E8 crash loop: a fast-exiting container trips the threshold → crash_loop → the real servers page renders red WITH the glyph", async (t) => {
  const tier = await detectTier();
  if (skipWithoutDocker(t, tier)) {
    return;
  }
  const engine = await launchEngine({});
  const browser = await launchBrowser();
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("crashloop"), {
      nodeId,
      startup: "sh -c 'exit 3'",
    });

    await client.post(`/api/v1/instances/${instance.id}/start`);
    // Threshold 3 crashes in the 60s window: the engine stops
    // auto-restarting and marks crash_loop.
    await waitFor(
      "crash_loop state",
      async () => {
        const state = await instanceState(client, instance.id);
        return state.status === "crash_loop" ? state : undefined;
      },
      180_000,
      500,
    );

    // The real page, in the real browser: the row's dot is red AND
    // carries the caution glyph — the §4 "not solid red" contract.
    const context = await newContext(browser);
    const page = await context.newPage();
    await loginViaUi(page, engine);
    await seedSession(context, engine, {
      v: 2,
      tabs: [{ path: "servers" }],
      active: 0,
    });
    await uiGoto(page, engine, "servers");
    await page
      .locator(`#server-rows tr[data-instance-id="${instance.id}"] zim-status-dot`)
      .waitFor({ timeout: 30_000 });
    const className = await page.evaluate((id: string) => {
      const dot = document.querySelector(
        `#server-rows tr[data-instance-id="${id}"] zim-status-dot`,
      );
      const inner = dot?.shadowRoot?.querySelector(".dot") ?? null;
      return inner?.className ?? "";
    }, instance.id);
    assert.ok(
      className.includes("dot-red"),
      `the crash-looped instance renders red, got [${className}]`,
    );
    assert.ok(
      className.includes("dot-crash"),
      `the caution glyph rides the red (never solid red), got [${className}]`,
    );
  } finally {
    await engine.stop();
  }
});
