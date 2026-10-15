/**
 * Unit tests for the tab model and the session formats (§7):
 * reorder logic, close semantics, split pairing (M23), and
 * view-only restoration for both v1 and v2 snapshots.
 */

import test from "node:test";
import assert from "node:assert/strict";

import {
  LEGACY_SESSION_KEY,
  SESSION_KEY,
  TabModel,
  defaultSession,
  restoreSession,
  sessionKeyForWindow,
} from "../src/tabs";

test("add opens next to the active tab and activates it", () => {
  const model = new TabModel();
  const first = model.add("newtab");
  assert.equal(model.activeId, first.id);
  const second = model.add("servers");
  assert.deepEqual(model.tabs.map((t) => t.path), ["newtab", "servers"]);
  assert.equal(model.activeId, second.id);
});

test("add can open without activating (session restore)", () => {
  const model = new TabModel();
  const first = model.add("newtab", { activate: true });
  model.add("servers/x/console", { activate: false });
  assert.equal(model.activeId, first.id);
  assert.equal(model.size, 2);
});

test("close hands focus to the right neighbor, then the left", () => {
  const model = new TabModel();
  const a = model.add("newtab");
  const b = model.add("servers");
  const c = model.add("servers/x/console");
  model.activate(b.id);
  model.close(b.id);
  assert.equal(model.activeId, c.id); // right neighbor inherits
  model.close(c.id);
  assert.equal(model.activeId, a.id); // at the edge, the left one
});

test("closing the last tab empties the strip (caller's policy)", () => {
  const model = new TabModel();
  const only = model.add("newtab");
  assert.equal(model.close(only.id), null);
  assert.equal(model.size, 0);
});

test("move reorders (drag-reorder)", () => {
  const model = new TabModel();
  const a = model.add("a");
  const b = model.add("b");
  const c = model.add("c");
  model.activate(a.id);
  model.move(c.id, 0);
  assert.deepEqual(model.tabs.map((t) => t.path), ["c", "a", "b"]);
  model.move(a.id, 2);
  assert.deepEqual(model.tabs.map((t) => t.path), ["c", "b", "a"]);
  model.move(b.id, 99); // clamped to the end
  assert.deepEqual(model.tabs.map((t) => t.path), ["c", "a", "b"]);
  assert.equal(model.activeId, a.id); // identity, not position
});

test("updatePath tracks omnibox navigation and pane resync", () => {
  const model = new TabModel();
  const tab = model.add("newtab");
  assert.equal(model.updatePath(tab.id, "servers"), true);
  assert.equal(model.updatePath(tab.id, "servers"), false); // no-op
  assert.equal(model.tabs[0].path, "servers");
});

// ── Splits (M23, §8) ─────────────────────────────────────────────

test("setSplit pairs a tab and clamps the ratio", () => {
  const model = new TabModel();
  const tab = model.add("servers/x/console");
  assert.equal(model.setSplit(tab.id, { path: "servers/y/files", ratio: 0.95 }), true);
  const split = model.splitOf(tab.id);
  assert.notEqual(split, null);
  assert.equal(split!.path, "servers/y/files");
  assert.equal(split!.ratio, 0.8); // clamped to the pane window
});

test("setSplit(null) closes the pairing; collapse keeps the other pane", () => {
  const model = new TabModel();
  const tab = model.add("servers/x/console");
  model.setSplit(tab.id, { path: "servers/y/files", ratio: 0.4 });
  assert.equal(model.setSplit(tab.id, null), true);
  assert.equal(model.splitOf(tab.id), null);
  assert.equal(model.tabs[0].path, "servers/x/console");

  model.setSplit(tab.id, { path: "servers/y/files", ratio: 0.4 });
  assert.equal(model.collapseSplitToSecondary(tab.id), true);
  assert.equal(model.tabs[0].path, "servers/y/files"); // survivor takes over
  assert.equal(model.splitOf(tab.id), null);
});

test("updateSplitPath and updateSplitRatio track the secondary pane", () => {
  const model = new TabModel();
  const tab = model.add("servers/x/console");
  model.setSplit(tab.id, { path: "servers/y/files", ratio: 0.5 });
  assert.equal(model.updateSplitPath(tab.id, "servers/y/metrics"), true);
  assert.equal(model.updateSplitPath(tab.id, "servers/y/metrics"), false);
  assert.equal(model.updateSplitRatio(tab.id, -3), true); // clamped
  assert.equal(model.splitOf(tab.id)!.ratio, 0.2);
});

// ── Serialization (v2) ───────────────────────────────────────────

test("serialize writes v2 entries, splits included, with tidy ratios", () => {
  const model = new TabModel();
  model.add("newtab");
  const b = model.add("servers");
  model.add("servers/x/console");
  model.setSplit(b.id, { path: "servers/x/files", ratio: 0.3333333 });
  model.activate(b.id);
  assert.deepStrictEqual(model.serialize(), {
    v: 2,
    tabs: [
      { path: "newtab" },
      { path: "servers", split: { path: "servers/x/files", ratio: 0.333 } },
      { path: "servers/x/console" },
    ],
    active: 1,
  });
});

test("restoreSession revives a well-formed v2 session with splits", () => {
  const revived = restoreSession({
    v: 2,
    tabs: [
      { path: "newtab" },
      { path: "servers/9f2c/console", split: { path: "servers/9f2c/files", ratio: 0.5 } },
      { path: "servers" },
    ],
    active: 1,
  });
  assert.notEqual(revived, null);
  assert.deepEqual(revived!.tabs[1], {
    path: "servers/9f2c/console",
    split: { path: "servers/9f2c/files", ratio: 0.5 },
  });
  assert.equal(revived!.active, 1);
});

test("restoreSession still revives legacy v1 snapshots (upgrade path)", () => {
  const revived = restoreSession({
    v: 1,
    tabs: ["newtab", "servers/9f2c/console", "servers"],
    active: 1,
  });
  assert.notEqual(revived, null);
  assert.deepEqual(revived!.tabs.map((t) => t.path), ["newtab", "servers/9f2c/console", "servers"]);
  assert.equal(revived!.tabs.every((t) => t.split === undefined), true);
});

test("restoreSession clamps a stale active index and drifted ratios", () => {
  const revived = restoreSession({
    v: 2,
    tabs: [{ path: "newtab", split: { path: "servers", ratio: 0.99 } }],
    active: 9,
  });
  assert.notEqual(revived, null);
  assert.equal(revived!.active, 0);
  assert.equal(revived!.tabs[0].split!.ratio, 0.8); // drift clamped, not rejected
});

test("restoreSession tolerates duplicate tabs (views, not processes)", () => {
  const revived = restoreSession({
    v: 2,
    tabs: [
      { path: "servers/x/console" },
      { path: "servers/x/console", split: { path: "servers/x/console", ratio: 0.5 } },
    ],
    active: 0,
  });
  assert.notEqual(revived, null);
  assert.equal(revived!.tabs.length, 2);
});

test("restoreSession rejects corruption instead of guessing", () => {
  assert.equal(restoreSession(null), null);
  assert.equal(restoreSession("nope"), null);
  assert.equal(restoreSession({ v: 3, tabs: [{ path: "newtab" }], active: 0 }), null);
  assert.equal(restoreSession({ v: 2, tabs: [], active: 0 }), null);
  assert.equal(restoreSession({ v: 2, tabs: "newtab", active: 0 }), null);
  // One unsafe path voids the whole session (a dropped middle tab
  // would silently renumber `active`).
  assert.equal(restoreSession({ v: 2, tabs: [{ path: "newtab" }, { path: "servers/../x" }], active: 1 }), null);
  assert.equal(restoreSession({ v: 2, tabs: [{ path: "newtab" }, { path: "servers?x=1" }], active: 0 }), null);
  assert.equal(restoreSession({ v: 2, active: 0, tabs: new Array(65).fill({ path: "newtab" }) }), null);
  // Corrupt splits void the session too: a missing pane would
  // silently change what restores.
  assert.equal(
    restoreSession({ v: 2, tabs: [{ path: "newtab", split: { path: "servers/../x", ratio: 0.5 } }] },),
    null,
  );
  assert.equal(
    restoreSession({ v: 2, tabs: [{ path: "newtab", split: { path: "servers", ratio: "half" } }] }),
    null,
  );
  assert.equal(restoreSession({ v: 2, tabs: [{ path: "newtab", split: null }] }), null);
  assert.equal(restoreSession({ v: 2, tabs: [null as unknown as { path: string }] }), null);
});

test("the default session is one new tab", () => {
  assert.deepStrictEqual(defaultSession(), { tabs: [{ path: "newtab" }], active: 0 });
});

// ── Per-window session keys (M23 multi-window) ───────────────────

test("session keys are per-window and hostile labels fall back", () => {
  assert.equal(sessionKeyForWindow(null), SESSION_KEY);
  assert.equal(sessionKeyForWindow("main"), SESSION_KEY);
  assert.equal(sessionKeyForWindow("chrome-1"), `${SESSION_KEY}.w:chrome-1`);
  // Untrusted labels never reach deeper into storage.
  assert.equal(sessionKeyForWindow("chrome-1 x"), SESSION_KEY);
  assert.equal(sessionKeyForWindow("chrome/../../etc"), SESSION_KEY);
  assert.equal(sessionKeyForWindow("x".repeat(80)), SESSION_KEY);
  assert.notEqual(SESSION_KEY, LEGACY_SESSION_KEY);
});
