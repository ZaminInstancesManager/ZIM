/**
 * Unit tests for the tab model and the session format (§7):
 * reorder logic, close semantics, and view-only restoration.
 */

import test from "node:test";
import assert from "node:assert/strict";

import { TabModel, defaultSession, restoreSession } from "../src/tabs";

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

test("serialize writes zim:// paths and the active position", () => {
  const model = new TabModel();
  model.add("newtab");
  const b = model.add("servers");
  model.add("servers/x/console");
  model.activate(b.id);
  assert.deepStrictEqual(model.serialize(), {
    v: 1,
    tabs: ["newtab", "servers", "servers/x/console"],
    active: 1,
  });
});

test("restoreSession revives a well-formed session", () => {
  const revived = restoreSession({
    v: 1,
    tabs: ["newtab", "servers/9f2c/console", "servers"],
    active: 1,
  });
  assert.notEqual(revived, null);
  assert.deepEqual(revived!.paths, ["newtab", "servers/9f2c/console", "servers"]);
  assert.equal(revived!.active, 1);
});

test("restoreSession clamps a stale active index", () => {
  const revived = restoreSession({ v: 1, tabs: ["newtab"], active: 9 });
  assert.notEqual(revived, null);
  assert.equal(revived!.active, 0);
});

test("restoreSession tolerates duplicate tabs (views, not processes)", () => {
  const revived = restoreSession({
    v: 1,
    tabs: ["servers/x/console", "servers/x/console"],
    active: 0,
  });
  assert.notEqual(revived, null);
  assert.equal(revived!.paths.length, 2);
});

test("restoreSession rejects corruption instead of guessing", () => {
  assert.equal(restoreSession(null), null);
  assert.equal(restoreSession("nope"), null);
  assert.equal(restoreSession({ v: 2, tabs: ["newtab"], active: 0 }), null);
  assert.equal(restoreSession({ v: 1, tabs: [], active: 0 }), null);
  assert.equal(restoreSession({ v: 1, tabs: "newtab", active: 0 }), null);
  // One unsafe path voids the whole session (a dropped middle tab
  // would silently renumber `active`).
  assert.equal(restoreSession({ v: 1, tabs: ["newtab", "servers/../x"], active: 1 }), null);
  assert.equal(restoreSession({ v: 1, tabs: ["newtab", "servers?x=1"], active: 0 }), null);
  assert.equal(restoreSession({ v: 1, active: 0, tabs: new Array(65).fill("newtab") }), null);
  // Non-string entries and absurd lengths are corruption too.
  assert.equal(restoreSession({ v: 1, tabs: [null as unknown as string], active: 0 }), null);
});

test("the default session is one new tab", () => {
  assert.deepEqual(defaultSession(), { paths: ["newtab"], active: 0 });
});
