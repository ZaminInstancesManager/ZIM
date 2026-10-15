/**
 * The backups scenario (§1.4): create a backup of an instance whose
 * data dir exists (docker tier: started once, then stopped), watch
 * the durable Job seal the archive (sha256 + bytes), lock it
 * against deletion, and delete an unlocked one. Plus the honest
 * failure shape: a backup of an instance with no data dir fails the
 * job (and leaves cleanup pending), never a fake sealed row.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { launchEngine } from "../lib/engine.js";
import {
  adminClient,
  createInstance,
  firstNodeId,
  instanceState,
} from "../lib/api.js";
import { detectTier, skipWithoutDocker } from "../lib/tier.js";
import { uniqueName, waitFor } from "../lib/util.js";

test("backups: create → job seals the archive → list → lock → delete (docker tier)", async (t) => {
  const tier = await detectTier();
  if (skipWithoutDocker(t, tier)) {
    return;
  }
  const engine = await launchEngine({});
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("backup"), { nodeId });

    // The data dir only exists once a container has run: start,
    // wait for running, stop.
    await client.post(`/api/v1/instances/${instance.id}/start`);
    await waitFor(
      "running state",
      async () => {
        const state = await instanceState(client, instance.id);
        return state.status === "running" ? state : undefined;
      },
      120_000,
      1_000,
    );
    await client.post(`/api/v1/instances/${instance.id}/stop`);
    await waitFor(
      "stopped state",
      async () => {
        const state = await instanceState(client, instance.id);
        return state.status === "stopped" ? state : undefined;
      },
      120_000,
      1_000,
    );

    // Create: the body carries both ids.
    const created = await client.post(`/api/v1/instances/${instance.id}/backups`);
    const backupId = String(created.backup_id);
    assert.ok(backupId.length > 0, "create answered with a backup id");
    assert.ok(String(created.job_id).length > 0, "create answered with a job id");

    // The row seals when the job completes: sha256 + bytes land.
    await waitFor(
      "backup sealed",
      async () => {
        const list = await client.get(`/api/v1/instances/${instance.id}/backups`);
        const row = (list.data as Array<Record<string, unknown>>).find(
          (entry) => String(entry.id) === backupId,
        );
        if (row === undefined) {
          return undefined;
        }
        const sealed =
          typeof row.sha256_hash === "string" &&
          (row.sha256_hash as string).length === 64 &&
          typeof row.bytes === "number" &&
          (row.bytes as number) > 0;
        return sealed ? row : undefined;
      },
      60_000,
      500,
    );

    // Lock: a locked backup refuses deletion (§10.5).
    await client.post(`/api/v1/instances/${instance.id}/backups/${backupId}/lock`);
    let lockedRefusal = false;
    try {
      await client.del(`/api/v1/instances/${instance.id}/backups/${backupId}`);
    } catch (error) {
      lockedRefusal = (error as { status: number }).status === 409;
    }
    assert.ok(lockedRefusal, "deleting a locked backup must be a 409 refusal");

    // Unlock + delete: the row disappears.
    await client.del(`/api/v1/instances/${instance.id}/backups/${backupId}/lock`);
    await client.del(`/api/v1/instances/${instance.id}/backups/${backupId}`);
    const after = await client.get(`/api/v1/instances/${instance.id}/backups`);
    assert.equal(
      (after.data as Array<Record<string, unknown>>).some((row) => String(row.id) === backupId),
      false,
    );
  } finally {
    await engine.stop();
  }
});

test("backups: no data dir → the job fails honestly, never a sealed row", async () => {
  const engine = await launchEngine({});
  try {
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    // Never started: the instance data dir does not exist yet.
    const instance = await createInstance(client, uniqueName("nobackupdir"), { nodeId });

    const created = await client.post(`/api/v1/instances/${instance.id}/backups`);
    const jobId = String(created.job_id);

    // The job lands in a terminal failed state with the source
    // error surfaced — not a pending-forever row.
    await waitFor(
      "backup job failed",
      async () => {
        const job = await client.get(`/api/v1/jobs/${jobId}`);
        const state = String(job.state ?? job.status ?? "").toLowerCase();
        return state === "failed" || state === "cancelled" ? state : undefined;
      },
      60_000,
      500,
    );

    // And the backup row never sealed.
    const list = await client.get(`/api/v1/instances/${instance.id}/backups`);
    const row = (list.data as Array<Record<string, unknown>>).find(
      (entry) => String(entry.id) === String(created.backup_id),
    );
    assert.ok(row !== undefined, "the failed backup's row exists");
    assert.equal(row?.sha256_hash ?? null, null, "no sha256 without a real archive");
  } finally {
    await engine.stop();
  }
});
