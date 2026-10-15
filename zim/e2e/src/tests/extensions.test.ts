/**
 * The extensions scenario (§1.4): install the repo's own sample
 * addon (packaged by the authoring script CI runs), enable it, see
 * its per-instance surface served under `/zim/`, see it listed in
 * the management page, then disable and uninstall. The admin's
 * upload is the approval (AD-29) — no signature required.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { join } from "node:path";
import { launchEngine } from "../lib/engine.js";
import { adminClient, createInstance, firstNodeId } from "../lib/api.js";
import { launchBrowser, newContext, loginViaUi, uiGoto } from "../lib/browser.js";
import { repoRoot, uniqueName } from "../lib/util.js";

const ADDON_ID = "org.example.zim-addon.hello-zim";

/** The packaged sample (built by CI's pre-step; env overridable). */
function helloPackagePath(): string {
  return process.env.ZIM_E2E_HELLO_PKG ?? join(repoRoot(), "zim", "e2e", "hello-zim.zimpkg");
}

test("extensions: install → enable → surface served → listed in the UI → disable → uninstall", async (t) => {
  const path = helloPackagePath();
  if (!existsSync(path)) {
    t.skip(`sample addon package not built (${path}); run package-addon.mjs build first`);
    return;
  }
  const engine = await launchEngine({});
  const browser = await launchBrowser();
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("hello"), { nodeId });

    // Fresh engine: only the bundled audit-log viewer is installed.
    const before = await client.get("/api/v1/extensions");
    assert.ok(
      (before.data as Array<Record<string, unknown>>).some(
        (row) => String(row.id) === "org.zaminmc.extension.audit-log",
      ),
      "the bundled audit-log addon is seeded",
    );

    // Install the packaged sample (admin upload = approval). The
    // response is the AddonSummary: id, version, enabled, ….
    const bytes = await import("node:fs/promises").then((fs) => fs.readFile(path));
    const installed = await client.postRaw(
      "/api/v1/extensions",
      bytes,
      "application/zip",
    );
    assert.equal(String(installed.id), ADDON_ID);

    // Enable it, then its declared surface serves real bytes.
    await client.post(`/api/v1/extensions/${ADDON_ID}/enable`);
    const surface = await client.tryGet(`/zim/servers/${instance.id}/hello`);
    assert.equal(surface.status, 200);
    assert.match(surface.body, /hello/i);

    // The management page lists it with its declared surface.
    const context = await newContext(browser);
    const page = await context.newPage();
    await loginViaUi(page, engine);
    await uiGoto(page, engine, "extensions");
    await page
      .getByText(/Hello ZIM/i)
      .first()
      .waitFor({ timeout: 30_000 });
    await context.close();

    // Disable: the surface falls back (404 — no addon answers it).
    await client.post(`/api/v1/extensions/${ADDON_ID}/disable`);
    const fallback = await client.tryGet(`/zim/servers/${instance.id}/hello`);
    assert.equal(fallback.status, 404);

    // Uninstall: gone from the list.
    await client.del(`/api/v1/extensions/${ADDON_ID}`);
    const after = await client.get("/api/v1/extensions");
    assert.equal(
      (after.data as Array<Record<string, unknown>>).some((row) => String(row.id) === ADDON_ID),
      false,
    );
  } finally {
    await browser.close();
    await engine.stop();
  }
});
