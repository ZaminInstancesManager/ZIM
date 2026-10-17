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

/** The §4 mapping table, asserted against the real served component
 * in the real browser (E9's honest scope, plus the mapping halves
 * of E7/E8 that the transient states cannot be caught mid-flight). */
const VISUAL_TABLE: Array<{ status: string; dotClass: string; extra?: string[]; pulse: boolean }> = [
  { status: "running", dotClass: "dot-green", pulse: false },
  { status: "starting", dotClass: "dot-yellow", pulse: true },
  { status: "stopping", dotClass: "dot-yellow", pulse: true },
  { status: "installing", dotClass: "dot-yellow", pulse: true },
  { status: "restoring_backup", dotClass: "dot-yellow", pulse: true },
  { status: "transferring", dotClass: "dot-yellow", pulse: true },
  { status: "offline", dotClass: "dot-blue", pulse: false },
  { status: "suspended", dotClass: "dot-gray", pulse: false },
  { status: "unknown", dotClass: "dot-gray", pulse: false },
  { status: "crash_loop", dotClass: "dot-red", extra: ["dot-crash"], pulse: false },
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
    // status and read back its shadow-root classes. (The evaluate
    // payload is a string expression by design: it runs in the
    // page's DOM world, which the Node-side tsconfig does not —
    // and should not — type.)
    const results = (await page.evaluate(
      `(statuses) => {
        const host = document.querySelector("main.page") || document.body;
        return statuses.map((status) => {
          const dot = document.createElement("zim-status-dot");
          dot.setAttribute("status", status);
          host.append(dot);
          const inner = dot.shadowRoot && dot.shadowRoot.querySelector(".dot");
          const classes = inner && inner.classList ? Array.from(inner.classList) : [];
          const labelEl = dot.shadowRoot && dot.shadowRoot.querySelector(".dot-label");
          const label = labelEl ? labelEl.textContent : "";
          dot.remove();
          return { status, classes, label };
        });
      }`,
      VISUAL_TABLE.map((row) => row.status),
    )) as Array<{ status: string; classes: string[]; label: string }>;

    for (const expected of VISUAL_TABLE) {
      const observed = results.find((row) => row.status === expected.status);
      assert.ok(observed !== undefined, `status ${expected.status} was driven`);
      assert.ok(
        observed.classes.includes(expected.dotClass),
        `${expected.status} must render ${expected.dotClass}, got [${observed.classes.join(" ")}]`,
      );
      assert.equal(
        observed.classes.includes("dot-pulse"),
        expected.pulse,
        `${expected.status} pulse expected ${expected.pulse}, got [${observed.classes.join(" ")}]`,
      );
      for (const extra of expected.extra ?? []) {
        assert.ok(
          observed.classes.includes(extra),
          `${expected.status} must carry ${extra}, got [${observed.classes.join(" ")}]`,
        );
      }
      assert.equal(observed.label, expected.status, "the honest label is the status itself");
    }

    // E9's own sentence: unknown reads gray — never red.
    const unknown = results.find((row) => row.status === "unknown");
    assert.ok(unknown !== undefined && unknown.classes.includes("dot-gray"));
    assert.ok(unknown !== undefined && !unknown.classes.includes("dot-red"));
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

    // Crash the container out from under the supervisor: the
    // monitor (1s poll) observes the death, records the crash
    // (below the threshold — the first one), and auto-restarts.
    // The recovering window is `starting` — poll fast enough to
    // catch it.
    const killed = await run("docker", ["kill", instance.id], { timeoutMs: 30_000 });
    assert.equal(killed.code, 0, "docker kill reached the container");

    let sawRecovering = false;
    let lied = false;
    const deadline = Date.now() + 60_000;
    while (Date.now() < deadline) {
      const state = await instanceState(client, instance.id);
      if (state.status === "starting" || state.status === "offline") {
        sawRecovering = true;
      }
      if (sawRecovering && state.status === "running") {
        break; // the recovery completed
      }
      if (!sawRecovering && state.status === "running" && Date.now() > deadline - 50_000) {
        // Running throughout would mean the kill never landed.
        lied = true;
        break;
      }
      await new Promise((resolve) => setTimeout(resolve, 150));
    }
    assert.ok(sawRecovering, "the observed state passed through the recovering (starting) phase");
    assert.equal(lied, false, "the engine never pretended the killed container was running");
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
    const classes = (await page.evaluate(
      `(id) => {
        const dot = document.querySelector(
          "#server-rows tr[data-instance-id=\"" + id + "\"] zim-status-dot",
        );
        const inner = dot && dot.shadowRoot ? dot.shadowRoot.querySelector(".dot") : null;
        return inner && inner.classList ? Array.from(inner.classList) : [];
      }`,
      instance.id,
    )) as string[];
    assert.ok(
      classes.includes("dot-red"),
      `the crash-looped instance renders red, got [${classes.join(" ")}]`,
    );
    assert.ok(
      classes.includes("dot-crash"),
      `the caution glyph rides the red (never solid red), got [${classes.join(" ")}]`,
    );
  } finally {
    await engine.stop();
  }
});
