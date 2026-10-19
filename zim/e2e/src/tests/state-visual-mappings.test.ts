/**
 * The E7/E8/E9 visual state mappings (TEST_STRATEGY §2, the M27
 * nightly failure-sweep family) — what the §4 status vocabulary
 * LOOKS like, driven as real as the single-node engine allows:
 *
 * - **E7 (recovering)**: a real container is `docker kill`ed; the
 *   supervisor's crash handling auto-restarts it (below the
 *   crash-loop threshold) and the observed status passes through
 *   `starting` — the yellow "recovering" phase — before running
 *   again. The transient window is observed through the §11 event
 *   stream (M30 CI fix 13): `instance.state_change` events queue
 *   for the subscriber and cannot be blinked past the way an HTTP
 *   poll can, so the recovery sequence is asserted as recorded
 *   transitions — death, yellow phase, return — never a straight
 *   offline → running leap. The mapping itself (`starting` renders
 *   yellow) is asserted against the real served component in this
 *   file's mapping table below.
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
import { awaitFirstEvent, openEventStream } from "../lib/stream.js";

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
  // The stream recorder must close even when the test fails before
  // its own cleanup line — a leaked socket holds the engine's
  // subscriber slot (and, through it, the engine process).
  let recorder: Awaited<ReturnType<typeof openEventStream>> | undefined;
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

    // M30 CI fix 13: transitions are observed through the §11
    // event stream, not through polling. The recovering window —
    // `offline` → `starting` — can pass in a few hundred
    // milliseconds under the suite's parallel load, and a poll can
    // blink straight past it (run 51: sixty seconds of polls on
    // ubuntu without one catching the yellow phase, while the
    // Windows and macOS legs caught it). `instance.state_change`
    // publishes from the engine's own `set_observed` on every
    // status-vocabulary change, queues for the subscriber, and
    // arrives in generation order — an event cannot be missed by
    // timing. Subscribe BEFORE the crash, warm the instrument on
    // the running instance's own traffic, and assert the recorded
    // sequence afterwards.
    recorder = await openEventStream(engine, client, instance.id);
    await awaitFirstEvent(recorder, 30_000);

    // Crash the container out from under the supervisor (the
    // CONTAINER name is the instance UUID — not the row id): the
    // monitor (1s poll) observes the death, records the crash
    // (below the threshold — the first one), and auto-restarts.
    // The recovering window is `starting`, and it is now pinned
    // in the event log regardless of how briefly it lasts.
    const killed = await run("docker", ["kill", instance.uuid], { timeoutMs: 30_000 });
    assert.equal(
      killed.code,
      0,
      `docker kill reached the container (stderr: ${killed.stderr.slice(0, 200)})`,
    );

    // The recovery sequence, as the stream delivered it. The
    // engine's own path is running → offline (the crash handling)
    // → starting (the auto-restart's apply_start) → running; the
    // sequence is asserted as "a death was recorded, the
    // recovering phase was passed through, and running returned —
    // never a straight offline → running leap".
    await waitFor(
      "the recovery sequence on the event stream",
      async () => {
        const tos = recorder!.stateChanges().map((change) => change.to);
        const sawDeath = tos.includes("offline") || tos.includes("starting");
        const startingAt = tos.indexOf("starting");
        const runningAfter = startingAt >= 0 ? tos.indexOf("running", startingAt) : -1;
        return sawDeath && runningAfter > startingAt ? { tos } : undefined;
      },
      90_000,
      500,
    );

    // The instrument read fast enough to lose nothing (a dropped
    // event would make the sequence above unreliable evidence).
    assert.equal(
      recorder.droppedEvents(),
      0,
      "the stream recorder lost events to backpressure — the recorded sequence is incomplete evidence",
    );

    const transitions = recorder.stateChanges();
    const tos = transitions.map((change) => change.to);
    assert.ok(
      tos.includes("offline") || tos.includes("starting"),
      `the observed state passed through the recovering (starting) phase; transitions: ${JSON.stringify(transitions)}`,
    );
    const startingAt = tos.indexOf("starting");
    assert.ok(
      startingAt >= 0,
      `the recovering phase itself (starting) was observed; transitions: ${JSON.stringify(transitions)}`,
    );
    assert.ok(
      tos.indexOf("running", startingAt) > startingAt,
      `the auto-restart brought the instance back to running; transitions: ${JSON.stringify(transitions)}`,
    );
    // Never fake-running: a death may only return to running
    // THROUGH starting — a straight offline → running transition
    // would mean the engine reported the dead container as
    // running without ever passing the yellow phase.
    for (let i = 0; i < transitions.length - 1; i += 1) {
      assert.ok(
        !(transitions[i]!.from === "offline" && transitions[i]!.to === "running"),
        `a transition leapt straight from offline to running — the engine faked running: ${JSON.stringify(transitions)}`,
      );
    }

    // The steady end-state, through the poll channel where it
    // belongs: the API agrees the instance is running again.
    await waitFor(
      "running state after recovery",
      async () => {
        const state = await instanceState(client, instance.id);
        return state.status === "running" ? state : undefined;
      },
      30_000,
      500,
    );
  } finally {
    await recorder?.close();
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
