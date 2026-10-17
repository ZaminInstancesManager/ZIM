/**
 * The files scenario (§10.3, M28): the real file-API family over the
 * real engine — upload, list, download, create, rename, archive,
 * extract, delete, the §2.6 grammar refusals, and the grant table.
 * The surface is host-filesystem (no Docker dependency), so the
 * scenario runs identically on every tier of the two-tier runtime
 * model (AD-30): Linux, macOS, and Windows all exercise every step.
 */

import net from "node:net";
import { test } from "node:test";
import assert from "node:assert/strict";
import { launchEngine } from "../lib/engine.js";
import { adminClient, createInstance, firstNodeId } from "../lib/api.js";
import { launchBrowser, loginViaUi, newContext, uiGoto } from "../lib/browser.js";
import { uniqueName } from "../lib/util.js";

test("files: the full family round-trips over the real routes", async () => {
  const engine = await launchEngine({});
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("files"), { nodeId });
    const files = `/api/v1/instances/${instance.id}/files`;

    // A fresh instance lists an empty root (the engine creates the
    // data dir on first touch).
    const empty = await client.get(`${files}`);
    assert.equal(empty.entries.length, 0);
    assert.equal(empty.path, ".");

    // Upload, then list.
    const uploaded = await client.putRaw(`${files}/content?path=eula.txt`, "eula=true\n", "text/plain");
    assert.equal(uploaded.size, 10);
    const listing = await client.get(`${files}`);
    assert.equal(listing.entries.length, 1);
    assert.equal(listing.entries[0].name, "eula.txt");
    assert.equal(listing.entries[0].is_dir, false);
    assert.equal(listing.entries[0].size, 10);
    assert.equal(typeof listing.entries[0].modified, "string");

    // Download: raw bytes with the octet-stream content type.
    const downloaded = await client.getRaw(`${files}/content?path=eula.txt`);
    assert.equal(downloaded.contentType, "application/octet-stream");
    assert.equal(downloaded.bytes.toString("utf8"), "eula=true\n");

    // Create a directory + a nested upload, list the subdirectory.
    await client.post(`${files}/create`, { path: "plugins", is_dir: true });
    await client.putRaw(`${files}/content?path=plugins/config.yml`, "debug: false\n", "text/plain");
    const plugins = await client.get(`${files}?path=plugins`);
    assert.equal(plugins.entries.length, 1);
    assert.equal(plugins.entries[0].name, "config.yml");

    // Rename.
    const renamed = await client.post(`${files}/rename`, {
      old_path: "plugins/config.yml",
      new_path: "plugins/config.bak.yml",
    });
    assert.equal(renamed.new_path, "plugins/config.bak.yml");

    // Archive + extract round-trip.
    const archived = await client.post(`${files}/archive`, {
      paths: ["eula.txt", "plugins"],
      destination: "snapshot.tar.gz",
      format: "tar.gz",
    });
    assert.equal(archived.format, "tar.gz");
    assert.ok(archived.bytes > 0);
    const extracted = await client.post(`${files}/extract`, {
      archive_path: "snapshot.tar.gz",
      destination: "restored",
    });
    assert.ok(extracted.entries >= 3);
    const restored = await client.getRaw(`${files}/content?path=restored/eula.txt`);
    assert.equal(restored.bytes.toString("utf8"), "eula=true\n");

    // Delete the restored tree (recursive), then the honest 404.
    await client.del(`${files}?path=restored`);
    const missing = await client.tryGet(`${files}/content?path=restored/eula.txt`);
    assert.equal(missing.status, 404);
    assert.match(missing.body, /"code"\s*:\s*"not_found"/);
  } finally {
    await engine.stop();
  }
});

test("files: the §2.6 path grammar is refused with structured errors", async () => {
  const engine = await launchEngine({});
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("grammar"), { nodeId });
    const files = `/api/v1/instances/${instance.id}/files`;

    // Traversal and absolute paths are validation failures on `path`
    // (never a 500, never a silent escape).
    for (const hostile of ["../escape.txt", "/etc/passwd"]) {
      const encoded = encodeURIComponent(hostile);
      const outcome = await client.tryGet(`${files}/content?path=${encoded}`);
      assert.ok(
        outcome.status === 400 || outcome.status === 404,
        `path ${hostile} -> ${outcome.status} (${outcome.body})`,
      );
      if (outcome.status === 400) {
        assert.match(outcome.body, /"code"\s*:\s*"validation_failed"/);
        assert.match(outcome.body, /"field"\s*:\s*"path"/);
      }
    }

    // The instance root itself is structurally undeletable.
    const rootDelete = await client.tryPost(`${files}/rename`, {
      old_path: ".",
      new_path: "elsewhere",
    });
    assert.ok([400, 404].includes(rootDelete.status), `root rename -> ${rootDelete.status}`);
  } finally {
    await engine.stop();
  }
});

test("files: the upload channel takes bodies past the JSON limit", async () => {
  const engine = await launchEngine({});
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("channel"), { nodeId });
    const files = `/api/v1/instances/${instance.id}/files`;

    // 11 MB: past the router-wide 10 MB JSON limit, comfortably
    // inside the route's §13 5 GB upload channel.
    const payload = Buffer.alloc(11 * 1024 * 1024, 0xa5);
    const uploaded = await client.putRaw(`${files}/content?path=big.bin`, payload, "application/octet-stream");
    assert.equal(uploaded.size, 11 * 1024 * 1024);

    // The JSON routes keep their own ceiling (a small create works
    // after the big upload — the channel is upload-shaped).
    await client.post(`${files}/create`, { path: "small.txt" });
    const listing = await client.get(`${files}`);
    assert.equal(listing.entries.length, 2);
  } finally {
    await engine.stop();
  }
});

test("files: the file manager drives the real family in the browser", async () => {
  const engine = await launchEngine({});
  const browser = await launchBrowser();
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("files-ui"), { nodeId });

    // Seed a directory tree through the API (the UI starts from a
    // known state).
    const files = `/api/v1/instances/${instance.id}/files`;
    await client.post(`${files}/create`, { path: "worlds", is_dir: true });
    await client.putRaw(`${files}/content?path=worlds/level.dat`, "level", "application/octet-stream");

    const context = await newContext(browser);
    const page = await context.newPage();
    await loginViaUi(page, engine);
    await uiGoto(page, engine, `servers/${instance.id}/files`);

    // The listing renders the seeded tree with honest metadata.
    const rows = page.locator("#files-list tr[data-name]");
    await rows.first().waitFor({ state: "visible", timeout: 15_000 });
    await page.locator('tr[data-name="worlds"]').waitFor({ state: "visible" });
    const names = await rows.evaluateAll((nodes) =>
      nodes.map((node) => (node as unknown as { dataset: { name?: string } }).dataset.name),
    );
    assert.deepEqual(names, ["worlds"]);

    // Descend into the directory (the breadcrumb follows).
    await page.locator('tr[data-name="worlds"] a').click();
    await page.locator('tr[data-name="level.dat"]').waitFor({ state: "visible" });
    const breadcrumbText = await page.locator("#files-breadcrumb").innerText();
    assert.match(breadcrumbText, /worlds/);

    // Upload through the real file input.
    await page.setInputFiles("#upload-input", {
      name: "uploaded.txt",
      mimeType: "text/plain",
      buffer: Buffer.from("uploaded via the browser"),
    });
    await page.locator('tr[data-name="uploaded.txt"]').waitFor({ state: "visible" });

    // The engine really holds the bytes (the API agrees with the UI).
    const downloaded = await client.getRaw(`${files}/content?path=worlds/uploaded.txt`);
    assert.equal(downloaded.bytes.toString("utf8"), "uploaded via the browser");

    // Rename through the prompt dialog.
    const renameDialog = page.waitForEvent("dialog").then((dialog) => dialog.accept("renamed.txt"));
    await page.locator('tr[data-name="uploaded.txt"] button[aria-label="rename uploaded.txt"]').click();
    await renameDialog;
    await page.locator('tr[data-name="renamed.txt"]').waitFor({ state: "visible" });

    // Delete through the confirm dialog.
    const deleteDialog = page.waitForEvent("dialog").then((dialog) => dialog.accept());
    await page.locator('tr[data-name="renamed.txt"] button[aria-label="delete renamed.txt"]').click();
    await deleteDialog;
    await page.locator('tr[data-name="renamed.txt"]').waitFor({ state: "detached" });

    await context.close();
  } finally {
    await browser.close();
    await engine.stop();
  }
});

test("files: the SFTP transport answers on its own port (the banner probe)", async () => {
  const engine = await launchEngine({});
  try {
    const banner = await new Promise<string>((resolve, reject) => {
      const socket = net.createConnection({ host: "127.0.0.1", port: engine.sftpPort });
      const chunks: Buffer[] = [];
      socket.setTimeout(10_000);
      socket.on("data", (chunk: Buffer) => {
        chunks.push(chunk);
        const text = Buffer.concat(chunks).toString("utf8");
        if (text.includes("\n")) {
          socket.destroy();
          resolve(text.trim());
        }
      });
      socket.on("timeout", () => {
        socket.destroy();
        reject(new Error(`no SSH banner within 10s (got ${Buffer.concat(chunks).length} bytes)`));
      });
      socket.on("error", reject);
    });
    assert.match(banner, /^SSH-2\.0-ZaminZIM/);
  } finally {
    await engine.stop();
  }
});
