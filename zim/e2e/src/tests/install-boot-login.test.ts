/**
 * The "install ZIM" scenario (TEST_STRATEGY §1.4): a fresh data
 * dir is an install; the engine boots, bootstraps the admin, and
 * the user signs in through the real new-tab page. Also covers the
 * negative login path (the card's alert, no session issued) and
 * the discovery contract the desktop shell relies on.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { launchEngine } from "../lib/engine.js";
import { adminClient, makeClient } from "../lib/api.js";
import { launchBrowser, newContext, uiGoto } from "../lib/browser.js";

test("install: first boot bootstraps the admin and serves the new-tab shell", async () => {
  const engine = await launchEngine({});
  const browser = await launchBrowser();
  try {
    // The discovery contract: engine.json exists and names our port.
    const client = await adminClient(engine);
    const health = await client.get("/api/v1/health");
    assert.equal(health.status, "ok");
    assert.equal(health.api_version, "v1");

    // The page surface is live before any login (static shell).
    const context = await newContext(browser);
    const page = await context.newPage();
    const response = await page.goto(`${engine.origin}/zim/newtab`, {
      waitUntil: "domcontentloaded",
    });
    assert.equal(response?.status(), 200);
    assert.match(await page.title(), /ZIM/);

    await context.close();
  } finally {
    await browser.close();
    await engine.stop();
  }
});

test("install: the login card signs the bootstrap admin in through the real UI", async () => {
  const engine = await launchEngine({});
  const browser = await launchBrowser();
  try {
    const context = await newContext(browser);
    const page = await context.newPage();
    await uiGoto(page, engine, "newtab");

    // Unauthenticated: the card is visible, the server list is not.
    const card = page.locator("#login-card");
    await card.waitFor({ state: "visible", timeout: 15_000 });

    const form = page.locator("zim-login-form");
    await form.locator('input[type="email"]').fill(engine.admin.email);
    await form.locator('input[type="password"]').fill(engine.admin.password);
    await form.locator('button[type="submit"]').click();

    // Success hides the card (the authenticated signal — the new
    // tab's own state, not anything inside the now-hidden card).
    await card.waitFor({ state: "hidden", timeout: 15_000 });

    // The session cookie is real: a second tab starts signed in.
    const second = await context.newPage();
    await uiGoto(second, engine, "newtab");
    await second.locator("#login-card").waitFor({ state: "hidden", timeout: 15_000 });

    await context.close();
  } finally {
    await browser.close();
    await engine.stop();
  }
});

test("install: a wrong password surfaces the card's alert and issues no session", async () => {
  const engine = await launchEngine({});
  const browser = await launchBrowser();
  try {
    const context = await newContext(browser);
    const page = await context.newPage();
    await uiGoto(page, engine, "newtab");

    const form = page.locator("zim-login-form");
    await form.locator('input[type="email"]').waitFor({ state: "visible", timeout: 15_000 });
    await form.locator('input[type="email"]').fill(engine.admin.email);
    await form.locator('input[type="password"]').fill("definitely-not-it");
    await form.locator('button[type="submit"]').click();

    // The card stays; its role="alert" error becomes non-empty.
    await page
      .locator('zim-login-form p[role="alert"]')
      .filter({ hasText: /.+/ })
      .waitFor({ timeout: 15_000 });

    // And no session cookie was issued for the wrong password.
    const cookies = await context.cookies(engine.origin);
    assert.equal(cookies.filter((cookie) => cookie.name === "zim_session").length, 0);

    // The API agrees: unauthenticated calls are 401.
    const anonymous = makeClient(engine);
    const denied = await anonymous.tryGet("/api/v1/instances");
    assert.equal(denied.status, 401);

    await context.close();
  } finally {
    await browser.close();
    await engine.stop();
  }
});

test("install: graceful SIGTERM removes the discovery file", async () => {
  const engine = await launchEngine({});
  // stop() asserts exit code 0; the discovery file disappearing is
  // the shell's "engine is down" signal (discovery.rs contract).
  await engine.stop();
  const { existsSync } = await import("node:fs");
  assert.equal(existsSync(`${engine.dataDir}/engine.json`), false);
});
