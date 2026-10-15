/**
 * The custom-frontend scenario and invariant I16: a signed custom
 * package uploads, activates, and *serves* — then the gate refuses
 * broken packages (failing smoke, missing entrypoint) with the
 * active frontend untouched, and reset restores the default. The
 * engine-side boot-recovery path (a persisted active package that
 * fails validation on the next boot) is covered by the engine's own
 * integration suite; this E2E pins the user-visible halves.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { launchEngine } from "../lib/engine.js";
import { adminClient } from "../lib/api.js";
import { launchBrowser, newContext, loginViaUi, uiGoto } from "../lib/browser.js";
import { customFrontendZip } from "../lib/packages.js";

const DEFAULT_ID = "org.zaminmc.frontend.default";

test("custom frontend: signed package uploads, activates, serves; broken ones are refused (I16)", async () => {
  const engine = await launchEngine({});
  const browser = await launchBrowser();
  try {
    const client = await adminClient(engine);

    // Baseline: the default frontend is active.
    const active = await client.get("/api/v1/frontend/active");
    assert.equal(String(active.id), DEFAULT_ID);
    assert.equal(active.is_default, true);

    // Sign in BEFORE any activation — the login form lives on the
    // default new-tab page, and the session cookie persists in the
    // context for the whole scenario.
    const context = await newContext(browser);
    const page = await context.newPage();
    await loginViaUi(page, engine);

    // ── A good, dev-signed package uploads (201 + report).
    const good = customFrontendZip({ id: "org.e2e.frontend.good" });
    const report = await client.postRaw("/api/v1/frontend/upload", good.bytes, "application/zip");
    assert.equal(String(report.id), "org.e2e.frontend.good");

    // ── Activate it; the newtab surface serves the custom page.
    await client.post("/api/v1/frontend/active", { id: "org.e2e.frontend.good" });
    await uiGoto(page, engine, "newtab");
    await page.locator("#custom-marker").waitFor({ timeout: 30_000 });

    // ── The failing-smoke package is refused; the active package
    //    is unchanged and still serving.
    const badSmoke = customFrontendZip({
      id: "org.e2e.frontend.badsmoke",
      broken: "failing-smoke",
    });
    let smokeRefused = false;
    try {
      await client.postRaw("/api/v1/frontend/upload", badSmoke.bytes, "application/zip");
    } catch (error) {
      smokeRefused = (error as { status: number }).status === 400;
    }
    assert.ok(smokeRefused, "a package whose smoke gate fails must be refused");
    const stillActive = await client.get("/api/v1/frontend/active");
    assert.equal(String(stillActive.id), "org.e2e.frontend.good");
    await uiGoto(page, engine, "newtab");
    await page.locator("#custom-marker").waitFor({ timeout: 30_000 });

    // ── The missing-entrypoint package is refused the same way.
    const badEntry = customFrontendZip({
      id: "org.e2e.frontend.badentry",
      broken: "missing-entrypoint",
    });
    let entryRefused = false;
    try {
      await client.postRaw("/api/v1/frontend/upload", badEntry.bytes, "application/zip");
    } catch (error) {
      entryRefused = (error as { status: number }).status === 400;
    }
    assert.ok(entryRefused, "a package missing its declared entrypoint must be refused");

    // ── Reset: the default frontend is active and serving again.
    await client.post("/api/v1/frontend/reset");
    const restored = await client.get("/api/v1/frontend/active");
    assert.equal(String(restored.id), DEFAULT_ID);
    assert.equal(restored.is_default, true);
    await uiGoto(page, engine, "newtab");
    // The default new-tab renders its own markers, not the custom one
    // (the signed-in row — stable on a zero-instance engine).
    await page.locator("zim-login-form .who").waitFor({ timeout: 30_000 });
    assert.equal(await page.locator("#custom-marker").count(), 0);

    await context.close();
  } finally {
    await browser.close();
    await engine.stop();
  }
});
