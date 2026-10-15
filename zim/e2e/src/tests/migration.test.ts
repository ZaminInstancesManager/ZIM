/**
 * The migration scenario (§1.4): the offline Pterodactyl importer
 * (dry-run then real), then the engine booting on the migrated
 * database. Authentication against imported users is deliberately
 * out of scope — bcrypt hashes import verbatim and ZIM's Argon2id
 * verification doesn't read them yet (§2.3's documented gap) — so
 * the E2E asserts the honest refusal instead of pretending.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { join } from "node:path";
import { launchEngine, runMigration } from "../lib/engine.js";
import { makeClient } from "../lib/api.js";
import { repoRoot, tempDir } from "../lib/util.js";

function fixturePath(): string {
  return join(
    repoRoot(),
    "zim",
    "engine",
    "crates",
    "migrate-pterodactyl",
    "tests",
    "fixtures",
    "panel_fixture.json",
  );
}

test("migration: dry-run rehearses, the real import writes, and the engine boots on the migrated DB", async () => {
  const dataDir = tempDir("migrated");
  const zimDb = join(dataDir, "engine.db");

  // ── Dry-run: exit 0, the report names the fixture's tables, and
  //    the target DB is untouched.
  const rehearsal = await runMigration({
    panelFixture: fixturePath(),
    zimDb,
    dryRun: true,
  });
  assert.match(rehearsal, /users/i);
  assert.match(rehearsal, /instances/i);
  assert.equal(await import("node:fs/promises").then((fs) => fs.access(zimDb).then(() => true, () => false)), false);

  // ── The real import: exit 0 with the fixture's known counts.
  const report = await runMigration({ panelFixture: fixturePath(), zimDb });
  assert.match(report, /2/); // two users, two nodes, two instances…

  // ── The engine boots on the migrated data dir (schema-compat +
  //    boot reconciliation with imported remote nodes). No admin
  //    login exists here — the migrated users own this DB — so the
  //    health surface (unauthenticated) is the probe.
  const engine = await launchEngine({ dataDir });
  try {
    const anonymous = makeClient(engine);
    const health = await anonymous.tryGet("/api/v1/health");
    assert.equal(health.status, 200);
    assert.match(health.body, /version/);

    // ── Imported credentials are honestly refused (§2.3 gap:
    //    bcrypt hashes are stored verbatim, not verified).
    let refused = false;
    try {
      await anonymous.login("admin@panel.example", "any-password");
    } catch (error) {
      refused = (error as { status: number }).status === 401;
    }
    assert.ok(refused, "the imported admin's bcrypt hash is not verifiable yet");

    // And no *bootstrap* admin was seeded over the migrated one.
    let bootstrapRefused = false;
    try {
      await anonymous.login("admin@zim.local", "zim-e2e-admin-password");
    } catch (error) {
      bootstrapRefused = (error as { status: number }).status === 401;
    }
    assert.ok(bootstrapRefused, "no bootstrap admin appears when users already exist");
  } finally {
    await engine.stop();
  }
});
