/**
 * The database-family pure model (M31): the connection-string
 * grammar and the name budget the create form speaks — the pieces
 * of the §10.21 surface that have shape worth pinning.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { connectionString, nameBudget, type DatabaseRow } from "../src/api.js";

function row(connections: DatabaseRow["connections"]): DatabaseRow {
  return {
    id: "db-1",
    instance_id: "inst-1",
    host_id: "host-1",
    name: "s0123abcd_world",
    username: "u0123abcd_a1b2c3",
    remote: "%",
    max_connections: 0,
    connections,
    created_at: "2026-10-19T12:00:00Z",
    updated_at: "2026-10-19T12:00:00Z",
  };
}

test("the connection string is the joined host's address and port", () => {
  assert.equal(connectionString(row({ host: "db.internal", port: 3306 })), "db.internal:3306");
});

test("a degraded join degrades to null, never to a lying string", () => {
  // The host row was unavailable: the row's own fields are honest,
  // the connection string is unknown — not "undefined:0".
  assert.equal(connectionString(row({ host: "", port: 0 })), null);
  assert.equal(
    connectionString(
      row({ host: "db.internal", port: Number.NaN }),
    ),
    null,
  );
});

test("a missing connections object degrades to null", () => {
  const partial = row({ host: "db.internal", port: 3306 });
  delete (partial as Partial<DatabaseRow>).connections;
  assert.equal(connectionString(partial), null);
});

test("the name budget spends the namespace before the user's part", () => {
  // The Panel's 48-char budget minus `s{short}_` (short + 2).
  assert.equal(nameBudget("0123abcd"), 48 - 10);
  assert.equal(nameBudget("abcd"), 48 - 6);
});

test("a degenerate short uuid cannot make the budget negative", () => {
  assert.equal(nameBudget(""), 46);
  assert.ok(nameBudget("0123456789abcdef0123456789abcdef") >= 0);
});
