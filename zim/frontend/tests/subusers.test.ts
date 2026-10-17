/**
 * The subuser-family pure model (M29): the engine's
 * PermissionGrant vocabulary as the frontend serves it — every
 * name unique, every group non-empty, the flat list in the
 * grouped order (the checkbox form's contract).
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { ALL_GRANTS, GRANT_VOCABULARY } from "../src/api.js";

test("the grant vocabulary is grouped and non-empty", () => {
  assert.ok(GRANT_VOCABULARY.length >= 8, "the engine's enum groups survive");
  for (const group of GRANT_VOCABULARY) {
    assert.ok(group.grants.length > 0, `group ${group.group} is non-empty`);
    for (const grant of group.grants) {
      assert.match(grant, /^[a-z_.]+$/, `${grant} is a dotted lowercase name`);
    }
  }
});

test("every grant name is unique across the vocabulary", () => {
  assert.equal(new Set(ALL_GRANTS).size, ALL_GRANTS.length);
});

test("the flat list follows the grouped order", () => {
  const spread = GRANT_VOCABULARY.flatMap((entry) => entry.grants);
  assert.deepEqual(ALL_GRANTS, spread);
});

test("the vocabulary carries the families the management surfaces speak", () => {
  // Spot-checks against the engine's own as_str mapping: one per
  // family the §10.3-§10.7 surfaces gate on.
  for (const must of [
    "websocket.connect",
    "control.start",
    "file.read",
    "backup.read",
    "schedule.read",
    "user.read",
    "settings.update",
    "admin.instance",
    "database.view_password",
  ]) {
    assert.ok(ALL_GRANTS.includes(must), `${must} is in the vocabulary`);
  }
});
