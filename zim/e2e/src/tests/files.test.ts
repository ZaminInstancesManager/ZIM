/**
 * The files scenario (§1.4), scoped honestly: the file API surface
 * is declared but deliberately 501 at the engine boundary (the
 * files crate exists; the API wiring is M6's remaining scope). The
 * E2E pins the *current* contract — a structured not-implemented
 * error, never a silent 200 with a fake body — so the day it turns
 * real, this test fails loudly and gets rewritten into the real
 * scenario.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { launchEngine } from "../lib/engine.js";
import { adminClient, createInstance, firstNodeId } from "../lib/api.js";
import { uniqueName } from "../lib/util.js";

test("files: the file API answers the structured 501 contract on every route", async () => {
  const engine = await launchEngine({});
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("files"), { nodeId });

    const routes: Array<[string, string]> = [
      ["GET", `/api/v1/instances/${instance.id}/files`],
      ["GET", `/api/v1/instances/${instance.id}/files/content?path=e2e.txt`],
      ["PUT", `/api/v1/instances/${instance.id}/files/content?path=e2e.txt`],
      ["POST", `/api/v1/instances/${instance.id}/files/create`],
      ["DELETE", `/api/v1/instances/${instance.id}/files?path=e2e.txt`],
      ["POST", `/api/v1/instances/${instance.id}/files/archive`],
      ["POST", `/api/v1/instances/${instance.id}/files/extract`],
    ];

    for (const [method, path] of routes) {
      const outcome = await request(client, method, path);
      assert.equal(outcome.status, 501, `${method} ${path}`);
      // §6.1's shape: `{"error": {"code": "not_implemented", …}}`.
      assert.match(outcome.body, /"code"\s*:\s*"not_implemented"/);
      assert.match(outcome.body, /file/i);
    }
  } finally {
    await engine.stop();
  }
});

/** One method-routed request, capturing the refusal instead of throwing. */
async function request(
  client: Awaited<ReturnType<typeof adminClient>>,
  method: string,
  path: string,
): Promise<{ status: number; body: string }> {
  try {
    if (method === "GET") {
      return await client.tryGet(path);
    }
    if (method === "PUT") {
      return { status: 200, body: JSON.stringify(await client.put(path, { path: "e2e.txt" })) };
    }
    if (method === "DELETE") {
      return { status: 200, body: JSON.stringify(await client.del(path)) };
    }
    return { status: 200, body: JSON.stringify(await client.post(path, { path: "e2e.txt" })) };
  } catch (error) {
    const apiError = error as { status: number; body: string };
    return { status: apiError.status, body: apiError.body };
  }
}
