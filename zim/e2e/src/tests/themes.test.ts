/**
 * The themes scenario (§1.4 / §10.14a): upload a packaged theme,
 * see it listed, see an unknown token refused by the closed
 * vocabulary, and delete it. The upload body is a real ZIP with
 * `zim.theme.json` at the root, built in-memory by the harness.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { launchEngine } from "../lib/engine.js";
import { adminClient } from "../lib/api.js";
import { themeZip } from "../lib/packages.js";

test("themes: upload → list → vocabulary enforcement → delete", async () => {
  const engine = await launchEngine({});
  try {
    const client = await adminClient(engine);

    // A fresh engine lists no uploaded themes.
    const before = await client.get("/api/v1/themes");
    assert.equal((before.data as unknown[]).length, 0);

    // Upload a valid theme (tokens from the closed vocabulary).
    const bytes = themeZip("E2E Dusk", {
      "color.background": "#101216",
      "color.accent": "#e11d48",
    });
    const uploaded = await client.postRaw("/api/v1/themes", bytes, "application/zip");
    assert.ok(String(uploaded.id ?? uploaded.name ?? "").length > 0);
    const themeId = String(uploaded.id ?? uploaded.name);

    // It is listed.
    const listed = await client.get("/api/v1/themes");
    const rows = listed.data as Array<Record<string, unknown>>;
    assert.equal(rows.length, 1);
    assert.match(String(rows[0].name), /E2E Dusk/);

    // The vocabulary is closed: an unknown token is refused.
    const rogue = themeZip("Rogue", { "color.not.a.token": "#000000" });
    let refused = false;
    try {
      await client.postRaw("/api/v1/themes", rogue, "application/zip");
    } catch (error) {
      refused = (error as { status: number }).status === 400;
    }
    assert.ok(refused, "an unknown token must be a 400 refusal");
    const still = await client.get("/api/v1/themes");
    assert.equal((still.data as unknown[]).length, 1, "the refused upload installed nothing");

    // Delete: the list is empty again.
    await client.del(`/api/v1/themes/${encodeURIComponent(themeId)}`);
    const after = await client.get("/api/v1/themes");
    assert.equal((after.data as unknown[]).length, 0);
  } finally {
    await engine.stop();
  }
});
