/**
 * Unit tests for the NTP form model (M23 §9): create-form
 * validation, JSON import parsing, and the search predicate.
 */

import test from "node:test";
import assert from "node:assert/strict";

import { formFromCreate, parseInstanceJson, serverMatchesQuery } from "../src/instanceForm";

const NODE = "6f0c9ac8-6f2e-4d7a-9f9f-2d2b6f18bcb2";

// ── Create ───────────────────────────────────────────────────────

test("formFromCreate builds the engine request from raw input", () => {
  const { request, problems } = formFromCreate({
    name: "  survival  ",
    node_id: NODE,
    description: "  the main world  ",
    startup: "",
    image: undefined,
  });
  assert.deepEqual(problems, []);
  assert.equal(request.name, "survival"); // trimmed
  assert.equal(request.node_id, NODE);
  assert.equal(request.description, "the main world");
  assert.equal(request.startup, undefined); // blank means absent
  assert.equal(request.image, undefined);
});

test("formFromCreate attributes its problems to fields", () => {
  const { problems } = formFromCreate({ name: "", node_id: "" });
  assert.deepEqual(
    problems.map((p) => p.field).sort(),
    ["name", "node_id"],
  );
  const tooLong = formFromCreate({ name: "x".repeat(200), node_id: NODE });
  assert.deepEqual(tooLong.problems.map((p) => p.field), ["name"]);
});

// ── Import (JSON) ────────────────────────────────────────────────

test("parseInstanceJson accepts a full definition", () => {
  const { request, problems } = parseInstanceJson(
    JSON.stringify({
      name: "imported",
      node_id: NODE,
      description: "from a backup",
      egg_id: "egg-1",
      startup: "java -jar server.jar",
      image: "ghcr.io/parkervcp/yolks:java",
      limits: { memory: 2048, cpu: 100 },
      backup_limit: 3,
      crash_detection_enabled: false,
    }),
    "fallback-node",
  );
  assert.deepEqual(problems, []);
  assert.equal(request!.name, "imported");
  assert.equal(request!.node_id, NODE);
  assert.deepEqual(request!.limits, { memory: 2048, cpu: 100 });
  assert.equal(request!.backup_limit, 3);
  assert.equal(request!.crash_detection_enabled, false);
});

test("parseInstanceJson fills the node from the dialog when omitted", () => {
  const { request, problems } = parseInstanceJson(JSON.stringify({ name: "no-node" }), NODE);
  assert.deepEqual(problems, []);
  assert.equal(request!.node_id, NODE);
});

test("parseInstanceJson ignores unknown fields (engine contract shape)", () => {
  const { request } = parseInstanceJson(
    JSON.stringify({ name: "x", node_id: NODE, favorite_color: "blue", tags: ["a"] }),
    NODE,
  );
  assert.equal((request as unknown as Record<string, unknown>)["favorite_color"], undefined);
  assert.equal((request as unknown as Record<string, unknown>)["tags"], undefined);
});

test("parseInstanceJson rejects non-JSON, non-objects, and bad limits", () => {
  assert.equal(parseInstanceJson("", NODE).request, null);
  assert.equal(parseInstanceJson("not json", NODE).request, null);
  assert.equal(parseInstanceJson("[1,2,3]", NODE).request, null);
  assert.equal(parseInstanceJson("42", NODE).request, null);
  const badLimits = parseInstanceJson(
    JSON.stringify({ name: "x", node_id: NODE, limits: [1, 2] }),
    NODE,
  );
  assert.equal(badLimits.request, null);
  assert.deepEqual(badLimits.problems.map((p) => p.field), ["limits"]);
});

test("parseInstanceJson attributes missing name/node problems", () => {
  const { problems } = parseInstanceJson(JSON.stringify({}), NODE);
  assert.deepEqual(
    problems.map((p) => p.field).sort(),
    ["name"],
  ); // node came from the dialog default
  const noNodeAnywhere = parseInstanceJson(JSON.stringify({ name: "x" }), "");
  assert.deepEqual(
    noNodeAnywhere.problems.map((p) => p.field).sort(),
    ["node_id"],
  );
});

test("parseInstanceJson refuses absurd payloads", () => {
  const huge = "x".repeat(65 * 1024);
  const { request, problems } = parseInstanceJson(`{"name":"${huge}"}`, NODE);
  assert.equal(request, null);
  assert.deepEqual(problems.map((p) => p.field), ["json"]);
});

// ── The search/filter predicate ──────────────────────────────────

test("serverMatchesQuery matches names and ids, case-insensitively", () => {
  assert.equal(serverMatchesQuery("Survival", "abc123", "surv"), true);
  assert.equal(serverMatchesQuery("Survival", "abc123", "ABC"), true);
  assert.equal(serverMatchesQuery("Creative", "abc123", "surv"), false);
  assert.equal(serverMatchesQuery("Creative", "abc123", "  "), true); // empty = all
  assert.equal(serverMatchesQuery("Skyblock", "9f2c91", "9f2c"), true); // id match
});
