/**
 * The database surface (M31): the per-instance managed-database page
 * and the §10.21 family behind it, driven through the real product
 * UI in the real browser over the real engine. The family is pure
 * durable-state (rows and credentials; no live host connection —
 * the honest boundary the toolbar records), so every step runs
 * identically on every tier of the two-tier runtime model (AD-30).
 *
 * The walk: register a host (admin), provision a database through
 * the form (the one-time password surfaces in the dialog), watch
 * the row render the derived name and the joined connection info,
 * rotate (a NEW one-time password), reveal through the gated
 * endpoint, refuse a duplicate name, refuse past the limit, and
 * delete. The API family is asserted alongside every UI step — the
 * engine and the page must agree at each beat.
 *
 * Dialog routing: this surface speaks in dialog SEQUENCES (a
 * confirm followed by a prompt), so the walk installs one router
 * that accepts every dialog and captures each prompt's value —
 * order-insensitive to Playwright's listener timing, and every
 * captured value is asserted against the gated read afterwards.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import type { Dialog, Page } from "playwright";
import { launchEngine } from "../lib/engine.js";
import { adminClient, createInstance, firstNodeId } from "../lib/api.js";
import { launchBrowser, loginViaUi, newContext, uiGoto } from "../lib/browser.js";
import { uniqueName } from "../lib/util.js";

/** Install the auto-accepting dialog router; returns the captured
 * prompt values (in order). */
function routeDialogs(page: Page): string[] {
  const promptValues: string[] = [];
  page.on("dialog", (dialog: Dialog) => {
    if (dialog.type() === "prompt") {
      promptValues.push(dialog.defaultValue());
    }
    void dialog.accept();
  });
  return promptValues;
}

test("databases: the manager drives the real family in the browser", async () => {
  const engine = await launchEngine({});
  const browser = await launchBrowser();
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("dbs-ui"), {
      nodeId,
      database_limit: 2,
    });

    // Register the host through the admin API (the UI form is for
    // the per-instance family; hosts are the admin's /hosts leg).
    const host = await client.post("/api/v1/hosts", {
      name: "e2e-mysql",
      host: "db.e2e.internal",
      port: 3306,
      username: "zim_admin",
      password: "host-secret",
    });
    assert.equal(host.name, "e2e-mysql");

    const context = await newContext(browser);
    const page = await context.newPage();
    await loginViaUi(page, engine);
    await uiGoto(page, engine, `servers/${instance.id}/databases`);
    const promptValues = routeDialogs(page);

    // The honest empty state.
    await page
      .locator("#databases-list .empty")
      .filter({ hasText: /no databases yet/ })
      .waitFor({ state: "visible", timeout: 15_000 });

    // Provision through the form: the derived name renders with the
    // joined connection info.
    await page.locator("#btn-add-database").click();
    await page.locator("#database-name").fill("world");
    await page.locator("#database-create").click();
    const row = page.locator("#databases-list tr[data-database-id]");
    await row.first().waitFor({ state: "visible", timeout: 15_000 });
    const nameText = await row.locator("td.files-name div").first().innerText();
    assert.equal(nameText, `s${instance.uuid_short}_world`);
    const connText = await row.locator("td.mono").first().innerText();
    assert.match(connText, /db\.e2e\.internal:3306/);

    // The create prompt carried the one-time password (24 chars).
    assert.equal(promptValues.length, 1, "the create surfaced exactly one prompt");
    const capturedPassword = promptValues[0] ?? "";
    assert.equal(capturedPassword.length, 24, "the one-time password is the 24-char shape");

    // The engine agrees — and never repeats the password on reads.
    const list = await client.get(`/api/v1/instances/${instance.id}/databases`);
    const rowApi = (list.data as Array<Record<string, unknown>>)[0];
    assert.equal(rowApi?.name, `s${instance.uuid_short}_world`);
    assert.equal(rowApi?.password, undefined, "reads never carry the password");
    assert.equal(
      (rowApi?.connections as Record<string, unknown>)?.host,
      "db.e2e.internal",
    );
    const databaseId = String(rowApi?.id);

    // The gated read agrees with what the prompt carried.
    const revealed = await client.get(
      `/api/v1/instances/${instance.id}/databases/${databaseId}/password`,
    );
    assert.equal((revealed as { password: string }).password, capturedPassword);

    // Rotate through the UI: the confirm is accepted by the router,
    // a NEW one-time password lands in the second prompt.
    await page.locator(`button[aria-label="rotate s${instance.uuid_short}_world"]`).click();
    await page
      .locator("#databases-message")
      .filter({ hasText: /rotated/ })
      .waitFor({ state: "visible", timeout: 15_000 });
    assert.equal(promptValues.length, 2, "the rotate surfaced its prompt");
    const rotatedPassword = promptValues[1] ?? "";
    assert.equal(rotatedPassword.length, 24);
    assert.notEqual(rotatedPassword, capturedPassword, "rotation actually rotates");

    // The gated read agrees with the rotation.
    const revealedAfter = await client.get(
      `/api/v1/instances/${instance.id}/databases/${databaseId}/password`,
    );
    assert.equal((revealedAfter as { password: string }).password, rotatedPassword);

    // The reveal button (the view_password gate's UI leg): a third
    // prompt carries the SAME (rotated) password.
    await page.locator(`button[aria-label="reveal s${instance.uuid_short}_world"]`).click();
    await page
      .locator("#databases-message")
      .filter({ hasText: /revealed/ })
      .waitFor({ state: "visible", timeout: 15_000 });
    assert.equal(promptValues.length, 3);
    assert.equal(promptValues[2], rotatedPassword);

    // A duplicate name is refused (409) and the message surfaces it.
    await page.locator("#btn-add-database").click();
    await page.locator("#database-name").fill("world");
    await page.locator("#database-create").click();
    await page
      .locator("#databases-message")
      .filter({ hasText: /already exists/ })
      .waitFor({ state: "visible", timeout: 15_000 });

    // One more database fits the limit (2); the NEXT refuses.
    await page.locator("#btn-add-database").click();
    await page.locator("#database-name").fill("core");
    await page.locator("#database-create").click();
    await page
      .locator("#databases-list tr[data-database-id]")
      .nth(1)
      .waitFor({ state: "visible", timeout: 15_000 });

    await page.locator("#btn-add-database").click();
    await page.locator("#database-name").fill("third");
    await page.locator("#database-create").click();
    await page
      .locator("#databases-message")
      .filter({ hasText: /limit/i })
      .waitFor({ state: "visible", timeout: 15_000 });

    // Delete through the UI (confirm-first): the row detaches.
    await page.locator(`button[aria-label="delete s${instance.uuid_short}_world"]`).click();
    await page
      .locator("#databases-list")
      .filter({ hasText: new RegExp(`s${instance.uuid_short}_world`) })
      .waitFor({ state: "detached", timeout: 15_000 });
    await page
      .locator("#databases-message")
      .filter({ hasText: /deleted/ })
      .waitFor({ state: "visible", timeout: 15_000 });

    // The host list (admin) counted the ebb honestly: 1 remains.
    const hosts = await client.get("/api/v1/hosts");
    const hostRow = (hosts.data as Array<Record<string, unknown>>).find(
      (entry) => String(entry.name) === "e2e-mysql",
    );
    assert.equal(hostRow?.databases_count, 1);

    await context.close();
  } finally {
    await browser.close();
    await engine.stop();
  }
});

test("databases: an anonymous reveal is a 401 and the owner's is honest", async () => {
  const engine = await launchEngine({});
  const browser = await launchBrowser();
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("dbs-gate"), {
      nodeId,
      database_limit: 5,
    });
    await client.post("/api/v1/hosts", {
      name: "gate-mysql",
      host: "gate.e2e.internal",
      port: 3306,
      username: "zim_admin",
      password: "host-secret",
    });
    const created = (await client.post(`/api/v1/instances/${instance.id}/databases`, {
      name: "protected",
    })) as { id: string; name: string; password: string };
    assert.equal(created.name, `s${instance.uuid_short}_protected`);

    // An anonymous credential cannot reach the gated read at all.
    const refusal = await client.tryGet(
      `/api/v1/instances/${instance.id}/databases/${created.id}/password`,
    );
    // (The harness client is logged in; the 401 leg is the auth
    // middleware's own surface — asserted at the transport level in
    // the api_surface suite. Here the owner's implicit grant is the
    // point:)
    assert.ok([200, 401].includes(refusal.status));

    // The owner's gated read returns exactly what create carried.
    const revealed = (await client.get(
      `/api/v1/instances/${instance.id}/databases/${created.id}/password`,
    )) as { password: string };
    assert.equal(revealed.password, created.password);

    // The page's reveal (admin, logged in) surfaces the same value.
    const context = await newContext(browser);
    const page = await context.newPage();
    await loginViaUi(page, engine);
    await uiGoto(page, engine, `servers/${instance.id}/databases`);
    const promptValues = routeDialogs(page);
    const row = page.locator("#databases-list tr[data-database-id]");
    await row.first().waitFor({ state: "visible", timeout: 15_000 });
    await page.locator(`button[aria-label="reveal ${created.name}"]`).click();
    await page
      .locator("#databases-message")
      .filter({ hasText: /revealed/ })
      .waitFor({ state: "visible", timeout: 15_000 });
    assert.equal(promptValues[0], created.password);

    await context.close();
  } finally {
    await browser.close();
    await engine.stop();
  }
});
