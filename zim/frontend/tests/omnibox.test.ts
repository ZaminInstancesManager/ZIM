/**
 * Unit tests for the omnibox suggestion engine: the §3 surfaces,
 * instance matches, address-first ranking, and error states.
 */

import test from "node:test";
import assert from "node:assert/strict";

import { buildSuggestions } from "../src/omnibox";

const INSTANCES = [
  { id: "9f2c", name: "Survival" },
  { id: "aa11", name: "Creative Flat" },
];

const SIGNED_IN = { instances: INSTANCES, signedIn: true };
const ANON = { instances: [] as { id: string; name: string }[], signedIn: false };

test("empty query yields the default set: surfaces, then consoles", () => {
  const rows = buildSuggestions("", SIGNED_IN);
  assert.equal(rows[0].path, "newtab");
  assert.equal(rows[1].path, "servers");
  assert.equal(rows[2].title, "Survival");
  assert.equal(rows[2].path, "servers/9f2c/console");
});

test("empty query, anonymous: a sign-in hint, no dead instance rows", () => {
  const rows = buildSuggestions("", ANON);
  assert.equal(rows.length, 3);
  assert.equal(rows[2].kind, "info");
  assert.equal(rows[2].path, "newtab");
});

test("a typed address ranks first", () => {
  const rows = buildSuggestions("zim://servers", SIGNED_IN);
  assert.equal(rows[0].kind, "address");
  assert.equal(rows[0].path, "servers");
});

test("instance names and ids match", () => {
  const byName = buildSuggestions("surv", SIGNED_IN);
  assert.equal(byName.some((r) => r.path === "servers/9f2c/console"), true);
  const byId = buildSuggestions("aa11", SIGNED_IN);
  assert.equal(byId.some((r) => r.path === "servers/aa11/console"), true);
});

test("surface titles match too", () => {
  const rows = buildSuggestions("serv", SIGNED_IN);
  assert.equal(rows.some((r) => r.path === "servers"), true);
});

test("foreign schemes show the error row", () => {
  const rows = buildSuggestions("https://example.test/", SIGNED_IN);
  assert.equal(rows.length, 1);
  assert.equal(rows[0].kind, "error");
  assert.equal(rows[0].path, null);
});

test("unsafe characters show the error row", () => {
  for (const bad of ["zim://servers/with space", "zim://a?b", "zim://../x"]) {
    const rows = buildSuggestions(bad, SIGNED_IN);
    assert.equal(rows.length, 1, bad);
    assert.equal(rows[0].kind, "error", bad);
  }
});

test("no matches, signed in: the address row is still offered", () => {
  const rows = buildSuggestions("workloads/minecraft/new", SIGNED_IN);
  assert.equal(rows[0].kind, "address");
  assert.equal(rows[0].path, "workloads/minecraft/new");
});

test("no matches, anonymous: the typed address is still offered (Chrome's URL-first rule)", () => {
  const rows = buildSuggestions("zzz", ANON);
  assert.equal(rows.length, 1);
  assert.equal(rows[0].kind, "address");
  assert.equal(rows[0].path, "zzz");
});

test("a partial segment still offers the address row (no search engine to fall back to)", () => {
  // ZIM has no omnibox search backend: a safe-but-unknown segment
  // is offered as an address; Enter shows the engine's honest
  // "surface not declared" page. Matching rows answer alongside.
  const rows = buildSuggestions("ser", ANON);
  assert.equal(rows[0].kind, "address");
  assert.equal(rows[0].path, "ser");
});

test("scheme-less input works like Chrome's originless typing", () => {
  const rows = buildSuggestions("servers/9f2c/console", SIGNED_IN);
  assert.equal(rows[0].kind, "address");
  assert.equal(rows[0].path, "servers/9f2c/console");
});
