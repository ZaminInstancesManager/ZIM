/**
 * The file-family path helpers (M28): the §10.3 root-relative
 * grammar the API speaks — join, parent, and the root's `.` form.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { joinFilePath, parentFilePath } from "../src/api.js";

test("joinFilePath builds root-relative paths", () => {
  assert.equal(joinFilePath(".", "eula.txt"), "eula.txt");
  assert.equal(joinFilePath("", "eula.txt"), "eula.txt");
  assert.equal(joinFilePath("plugins", "config.yml"), "plugins/config.yml");
  assert.equal(joinFilePath("plugins/", "config.yml"), "plugins/config.yml");
  assert.equal(joinFilePath("worlds/world", "level.dat"), "worlds/world/level.dat");
});

test("parentFilePath walks up to the root's `.` form", () => {
  assert.equal(parentFilePath("eula.txt"), ".");
  assert.equal(parentFilePath("plugins/config.yml"), "plugins");
  assert.equal(parentFilePath("worlds/world/level.dat"), "worlds/world");
  assert.equal(parentFilePath("trailing/slash/"), "trailing");
});

test("the helpers round-trip a listing walk", () => {
  // Listing "a/b" yields entry "c.txt" → path "a/b/c.txt" → parent
  // "a/b" — the breadcrumb's whole contract.
  const child = joinFilePath("a/b", "c.txt");
  assert.equal(child, "a/b/c.txt");
  assert.equal(parentFilePath(child), "a/b");
  assert.equal(joinFilePath(parentFilePath(child), "c.txt"), child);
});
