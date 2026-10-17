/**
 * The management surfaces (M29): the four per-instance pages the
 * milestone graduated — backups, schedules, subusers, settings —
 * driven through the real product UI in the real browser, over the
 * real engine. The surface families are host-filesystem or
 * durable-state (no Docker dependency for what these walks
 * exercise), so every step runs identically on every tier of the
 * two-tier runtime model (AD-30): Linux, macOS, and Windows.
 *
 * The settings walk also drives §8's concurrency gate for real: a
 * concurrent writer changes the row behind the page's back, and
 * the page's next save answers 412 with the honest reload message.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { launchEngine } from "../lib/engine.js";
import { adminClient, createInstance, firstNodeId } from "../lib/api.js";
import { launchBrowser, loginViaUi, newContext, uiGoto } from "../lib/browser.js";
import { uniqueName } from "../lib/util.js";

test("management: the backup manager drives the real family in the browser", async () => {
  const engine = await launchEngine({});
  const browser = await launchBrowser();
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("backups-ui"), { nodeId });

    // Seed the data dir through the files API (first touch creates
    // it — the same tree the backup pipeline archives).
    const files = `/api/v1/instances/${instance.id}/files`;
    await client.putRaw(`${files}/content?path=world.txt`, "the world", "text/plain");

    const context = await newContext(browser);
    const page = await context.newPage();
    await loginViaUi(page, engine);
    await uiGoto(page, engine, `servers/${instance.id}/backups`);

    // Back up now: the durable job seals the archive; the row
    // renders honest metadata.
    await page.locator("#btn-backup-now").click();
    const sealedRow = page.locator('#backups-list tr[data-sealed="true"]');
    await sealedRow.waitFor({ state: "visible", timeout: 60_000 });
    const fileName = await sealedRow.locator("td.files-name div").first().innerText();
    assert.match(fileName, /\.tar\.gz$/);

    // The engine agrees: the row is sealed (sha256 + bytes).
    const list = await client.get(`/api/v1/instances/${instance.id}/backups`);
    const row = (list.data as Array<Record<string, unknown>>).find(
      (entry) => String(entry.file_name) === fileName,
    );
    assert.ok(row !== undefined, "the API sees the UI-created backup");
    assert.equal(typeof row?.sha256_hash, "string");
    assert.ok((row?.sha256_hash as string).length === 64);

    // Lock through the UI.
    await page.locator(`button[aria-label="lock ${fileName}"]`).click();
    await page.locator('#backups-list tr[data-locked="true"]').waitFor({ state: "visible" });

    // Delete while locked: the confirm is accepted, the engine's
    // 409 refusal surfaces as the honest message.
    const refusalDialog = page.waitForEvent("dialog").then((dialog) => dialog.accept());
    await page.locator(`button[aria-label="delete ${fileName}"]`).click();
    await refusalDialog;
    await page
      .locator("#backups-message")
      .filter({ hasText: /refused.*locked|locked.*refus/i })
      .waitFor({ state: "visible", timeout: 15_000 });

    // Unlock, then delete for real: the row detaches.
    await page.locator(`button[aria-label="unlock ${fileName}"]`).click();
    await page.locator('#backups-list tr[data-locked="false"]').waitFor({ state: "visible" });
    const deleteDialog = page.waitForEvent("dialog").then((dialog) => dialog.accept());
    await page.locator(`button[aria-label="delete ${fileName}"]`).click();
    await deleteDialog;
    await page.locator("#backups-list tr").filter({ hasText: fileName }).waitFor({
      state: "detached",
      timeout: 15_000,
    });

    await context.close();
  } finally {
    await browser.close();
    await engine.stop();
  }
});

test("management: the schedule manager drives the real family in the browser", async () => {
  const engine = await launchEngine({});
  const browser = await launchBrowser();
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("sched-ui"), { nodeId });

    const context = await newContext(browser);
    const page = await context.newPage();
    await loginViaUi(page, engine);
    await uiGoto(page, engine, `servers/${instance.id}/schedules`);

    // The honest client-side cron preview: a truncated expression
    // refuses before the round-trip.
    await page.locator("#btn-new-schedule").click();
    await page.locator("#schedule-name").fill("nightly restart");
    await page.locator("#schedule-cron").fill("0 4 * *");
    await page.locator("#schedule-create").click();
    await page
      .locator("#schedules-message")
      .filter({ hasText: /5 non-empty fields/ })
      .waitFor({ state: "visible" });

    // A real expression creates the schedule; the row carries it.
    await page.locator("#schedule-cron").fill("0 4 * * *");
    await page.locator("#schedule-create").click();
    const row = page.locator('#schedules-list tr[data-schedule-id]');
    await row.first().waitFor({ state: "visible", timeout: 15_000 });
    assert.match(await row.locator("td.mgmt-cron").innerText(), /0 4 \* \* \*/);
    assert.equal(await row.getAttribute("data-active"), "true");

    // The engine agrees: one schedule, structurally valid.
    const list = await client.get(`/api/v1/instances/${instance.id}/schedules`);
    assert.equal((list.data as unknown[]).length, 1);

    // Pause through the UI (the is_active flag).
    await page.locator('button[aria-label^="pause "]').click();
    await page.locator('#schedules-list tr[data-active="false"]').waitFor({ state: "visible" });

    // Delete through the confirm dialog: the row detaches.
    const deleteDialog = page.waitForEvent("dialog").then((dialog) => dialog.accept());
    await page.locator('button[aria-label^="delete "]').click();
    await deleteDialog;
    await page.locator('#schedules-list tr[data-schedule-id]').waitFor({
      state: "detached",
      timeout: 15_000,
    });

    await context.close();
  } finally {
    await browser.close();
    await engine.stop();
  }
});

test("management: the subuser manager drives the real family in the browser", async () => {
  const engine = await launchEngine({});
  const browser = await launchBrowser();
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("subs-ui"), { nodeId });

    // A second real user (the subuser must exist).
    const created = await client.post("/api/v1/users", {
      email: `sub-${Date.now()}@zim.local`,
      username: `sub${Date.now()}`,
      password: "zim-e2e-subuser-password-12345",
    });
    const userId = String(created.id);
    const username = String(created.username);

    const context = await newContext(browser);
    const page = await context.newPage();
    await loginViaUi(page, engine);
    await uiGoto(page, engine, `servers/${instance.id}/subusers`);

    // Add through the form: the user id + one real grant checkbox.
    await page.locator("#btn-add-subuser").click();
    await page.locator("#subuser-id").fill(userId);
    await page.locator('#subuser-grants input[value="control.start"]').check();
    await page.locator("#subuser-create").click();
    const row = page.locator(`#subusers-list tr[data-user-id="${userId}"]`);
    await row.waitFor({ state: "visible", timeout: 15_000 });
    assert.match(await row.innerText(), new RegExp(username));

    // The engine agrees: one permission row with the one grant.
    const list = await client.get(`/api/v1/instances/${instance.id}/subusers`);
    assert.equal((list.data as unknown[]).length, 1);
    assert.deepEqual((list.data as Array<{ grants: string[] }>)[0].grants, ["control.start"]);

    // Replace the grant set through the prompt dialog.
    const editDialog = page
      .waitForEvent("dialog")
      .then((dialog) => dialog.accept("control.start,file.read"));
    await page.locator(`button[aria-label="edit grants for ${username}"]`).click();
    await editDialog;
    await page
      .locator(`#subusers-list tr[data-user-id="${userId}"]`)
      .filter({ hasText: "2 grants" })
      .waitFor({ state: "visible", timeout: 15_000 });

    // Remove through the confirm dialog: the row detaches.
    const removeDialog = page.waitForEvent("dialog").then((dialog) => dialog.accept());
    await page.locator(`button[aria-label="remove ${username}"]`).click();
    await removeDialog;
    await page.locator(`#subusers-list tr[data-user-id="${userId}"]`).waitFor({
      state: "detached",
      timeout: 15_000,
    });

    await context.close();
  } finally {
    await browser.close();
    await engine.stop();
  }
});

test("management: the settings surface saves under the §8 gate (and 412s honestly)", async () => {
  const engine = await launchEngine({});
  const browser = await launchBrowser();
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("settings-ui"), { nodeId });

    const context = await newContext(browser);
    const page = await context.newPage();
    await loginViaUi(page, engine);
    await uiGoto(page, engine, `servers/${instance.id}/settings`);

    // The form prefills from the row (name + the etag pill shows).
    const nameField = page.locator("#set-name");
    await nameField.waitFor({ state: "visible", timeout: 15_000 });
    assert.equal(await nameField.inputValue(), instance.name);
    await page.locator("#etag-pill").filter({ hasText: /inst-/ }).waitFor({ state: "visible" });

    // Rename and save: the engine's row agrees with the UI.
    const renamed = `${instance.name}-renamed`;
    await nameField.fill(renamed);
    await page.locator("#settings-save").click();
    await page
      .locator("#settings-message")
      .filter({ hasText: /saved as of/ })
      .waitFor({ state: "visible", timeout: 15_000 });
    const after = await client.get(`/api/v1/instances/${instance.id}`);
    assert.equal(String(after.name), renamed);

    // §8's lost-update race, for real: a concurrent writer changes
    // the row behind the page's back (the page's held ETag goes
    // stale), and the page's next save answers 412 with the honest
    // reload message — never a silent overwrite.
    const concurrent = await client.patchWithHeaders(
      `/api/v1/instances/${instance.id}`,
      { description: "changed behind the page's back" },
      { "If-Match": "*" },
    );
    assert.equal(concurrent.status, 200, `concurrent patch: ${concurrent.body.slice(0, 200)}`);

    await nameField.fill(`${renamed}-again`);
    await page.locator("#settings-save").click();
    await page
      .locator("#settings-message")
      .filter({ hasText: /412/ })
      .waitFor({ state: "visible", timeout: 15_000 });
    // The concurrent writer's change survived the refused save.
    const raced = await client.get(`/api/v1/instances/${instance.id}`);
    assert.equal(String(raced.description), "changed behind the page's back");
    assert.equal(String(raced.name), renamed);

    // Reload re-reads the row (the form reflects the truth again).
    await page.locator("#btn-reload").click();
    await page
      .locator("#settings-message")
      .filter({ hasText: /loaded as of/ })
      .waitFor({ state: "visible", timeout: 15_000 });
    assert.equal(await nameField.inputValue(), renamed);

    // The danger zone: delete under the same gate (two confirms),
    // then the page walks to the server list.
    const confirm1 = page.waitForEvent("dialog").then((dialog) => dialog.accept());
    const confirm2 = page.waitForEvent("dialog").then((dialog) => dialog.accept());
    await page.locator("#btn-delete").click();
    await confirm1;
    await confirm2;
    await page.waitForURL(/\/zim\/servers\/?$/, { timeout: 15_000 });

    // The instance is really gone.
    const missing = await client.tryGet(`/api/v1/instances/${instance.id}`);
    assert.equal(missing.status, 404);

    await context.close();
  } finally {
    await browser.close();
    await engine.stop();
  }
});
