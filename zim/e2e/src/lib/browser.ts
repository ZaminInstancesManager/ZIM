/**
 * The browser half of the harness: one headless Chromium per test
 * file, contexts that trust the engine's self-signed loopback
 * certificate (ignoreHTTPSErrors — the browser-side equivalent of
 * the shell installing the cert into the OS trust store), and the
 * UI-level login helper that drives the real new-tab login card.
 */

import { chromium, type Browser, type BrowserContext, type Page } from "playwright";
import type { EngineHandle } from "./engine.js";

/**
 * Launch the suite's headless Chromium.
 *
 * `--ignore-certificate-errors` is the harness's equivalent of the
 * shell's M21 trust-store installation: the engine serves a
 * self-signed loopback certificate by design, Playwright's
 * context-level `ignoreHTTPSErrors` covers page navigations, but a
 * page-initiated WSS handshake (`/api/v1/stream`) can still fail
 * TLS validation without the flag — the live console stream then
 * silently reconnect-loops and every live-delivery assertion
 * times out (the M25 console-stream lesson).
 */
export async function launchBrowser(): Promise<Browser> {
  return await chromium.launch({
    headless: true,
    args: ["--ignore-certificate-errors"],
  });
}

/** A fresh context trusting the engine origin's certificate. */
export async function newContext(browser: Browser): Promise<BrowserContext> {
  return await browser.newContext({
    ignoreHTTPSErrors: true,
    viewport: { width: 1440, height: 900 },
  });
}

/**
 * A persistent context (localStorage + cookies survive a
 * relaunch) — the session-restore family needs exactly that
 * browser-restart shape. `userDataDir` is the caller's scratch dir.
 */
export async function newPersistentContext(
  userDataDir: string,
): Promise<BrowserContext> {
  return await chromium.launchPersistentContext(userDataDir, {
    headless: true,
    ignoreHTTPSErrors: true,
    // The WSS-handshake half of the self-signed story (see
    // launchBrowser's note) — persistent contexts need it too:
    // session-restore's pages open live streams.
    args: ["--ignore-certificate-errors"],
    viewport: { width: 1440, height: 900 },
  });
}

/** Navigate to one `zim://` surface served at `/zim/{path}`. */
export async function uiGoto(page: Page, engine: EngineHandle, zimPath: string): Promise<void> {
  const clean = zimPath.replace(/^zim:\/\//, "").replace(/^\/+/, "");
  await page.goto(`${engine.origin}/zim/${clean}`, { waitUntil: "domcontentloaded" });
}

/**
 * Log in through the real UI (the new-tab login card). Wrong
 * credentials surface the form's `role="alert"` error; success
 * hides the card — the helper asserts the success shape.
 */
export async function loginViaUi(
  page: Page,
  engine: EngineHandle,
  options: { email?: string; password?: string } = {},
): Promise<void> {
  await uiGoto(page, engine, "newtab");
  const email = options.email ?? engine.admin.email;
  const password = options.password ?? engine.admin.password;

  const form = page.locator("zim-login-form");
  await form.locator('input[type="email"]').waitFor({ state: "visible", timeout: 15_000 });
  await form.locator('input[type="email"]').fill(email);
  await form.locator('input[type="password"]').fill(password);
  await form.locator('button[type="submit"]').click();

  // Success: the hosting page hides the login card (newtab.ts's
  // own whoami refresh — the card hiding IS the authenticated
  // signal; everything inside the hidden card reports hidden too).
  await page
    .locator("#login-card")
    .waitFor({ state: "hidden", timeout: 15_000 });
}

/** Seed a session snapshot into localStorage before first load. */
export async function seedSession(
  context: BrowserContext,
  engine: EngineHandle,
  snapshot: unknown,
): Promise<void> {
  const page = await context.newPage();
  // Any routed page pins the origin for localStorage; the new-tab
  // shell is the cheapest one (unauthenticated static shell).
  await uiGoto(page, engine, "newtab");
  await page.evaluate((value) => {
    // The callback runs *in the page*; the structural type keeps
    // the Node-side type-check DOM-lib-free.
    const storage = (
      globalThis as unknown as {
        localStorage?: { setItem(key: string, value: string): void };
      }
    ).localStorage;
    if (storage === undefined) {
      throw new Error("no localStorage on the engine origin");
    }
    storage.setItem("zim.session.v2", JSON.stringify(value));
  }, snapshot);
  await page.close();
}
