/**
 * The interrupted-backup scenario (M27's nightly failure-sweep
 * family): a SIGKILL mid-archive — the crash-reconciliation shape
 * for the durable-job engine (RUST_ENGINE.md §8, invariant 11).
 *
 * The engine is killed while a real backup of a real (fat) data
 * dir is in progress, then relaunched on the SAME data dir. The
 * honest outcomes, asserted:
 *
 * - the orphaned job is reconciled to `failed` (never stuck
 *   `in_progress`, never faked `succeeded`) with the restart named
 *   in the error detail;
 * - the backup row never seals (no sha256 — there is no honest
 *   archive);
 * - the partial `.part` file is cleaned up by the boot-time
 *   reconciliation (cleanup_pending does its work);
 * - the engine that comes back is healthy and can still run a
 *   backup to completion (the failure did not poison the pipeline).
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { join } from "node:path";
import { stat } from "node:fs/promises";
import { launchEngine } from "../lib/engine.js";
import { adminClient, createInstance, firstNodeId, instanceState } from "../lib/api.js";
import { detectTier, skipWithoutDocker } from "../lib/tier.js";
import { tempDir, uniqueName, waitFor } from "../lib/util.js";

/** A startup that plants a fat file in the mounted data dir before
 * ticking: the backup archive then takes long enough that a crash
 * mid-archive is a *reliable* race, not a lucky one. (bs is spelled
 * in bytes — every busybox agrees on 1048576, and the first CI run
 * suspects `1M` — and the file's presence is VERIFIED host-side
 * before the backup: if the container never wrote it, the archive
 * is instant and the interruption window never opens.) */
const FAT_STARTUP =
  "sh -c 'dd if=/dev/zero of=/home/container/big.bin bs=1048576 count=256; " +
  "while true; do echo zim-e2e-tick; sleep 1; done'";

/** A file's size, or null when absent. */
async function statFile(path: string): Promise<number | null> {
  try {
    return (await stat(path)).size;
  } catch {
    return null;
  }
}

/** Recursively collect `.part` files under `root` (missing root:
 * none). */
async function collectPartials(root: string): Promise<string[]> {
  const { readdir } = await import("node:fs/promises");
  try {
    const entries = await readdir(root, { withFileTypes: true });
    const nested = await Promise.all(
      entries.map((entry) =>
        entry.isDirectory()
          ? collectPartials(join(root, entry.name))
          : entry.name.endsWith(".part")
            ? Promise.resolve([join(root, entry.name)])
            : Promise.resolve([] as string[]),
      ),
    );
    return nested.flat();
  } catch {
    return [];
  }
}

test("interrupted backup: SIGKILL mid-archive → boot reconciliation fails the orphan honestly, cleanup runs, and the pipeline still works", async (t) => {
  const tier = await detectTier();
  if (skipWithoutDocker(t, tier)) {
    return;
  }
  const dataDir = tempDir("interrupted");
  const engine = await launchEngine({ dataDir });
  const port = engine.port;
  let client = await adminClient(engine);
  try {
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("interrupt"), {
      nodeId,
      startup: FAT_STARTUP,
    });

    // The container must have run (and written the fat file) before
    // the backup has anything to archive.
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
      "offline state",
      async () => {
        const state = await instanceState(client, instance.id);
        return state.status === "offline" ? state : undefined;
      },
      120_000,
      1_000,
    );

    // The fat file must exist host-side before the race is armed:
    // the instance data dir is <data_root>/instances/{uuid_short},
    // and a missing big.bin means the archive seals instantly and
    // there is no interruption window at all.
    const bigBin = join(dataDir, "instances", instance.uuid_short, "big.bin");
    await waitFor(
      "the fat file in the instance data dir",
      async () => {
        const stat = await statFile(bigBin);
        return stat !== null && stat >= 256 * 1024 * 1024 ? stat : undefined;
      },
      30_000,
      500,
    );

    // Create the backup, then crash the engine the moment the job
    // reports in_progress — the archive of 256 MiB is underway.
    const created = await client.post(`/api/v1/instances/${instance.id}/backups`);
    const jobId = String(created.job_id);
    const backupId = String(created.backup_id);
    await waitFor(
      "job in progress",
      async () => {
        const job = await client.get(`/api/v1/jobs/${jobId}`);
        return String(job.state ?? job.status ?? "").toLowerCase() === "in_progress"
          ? job
          : undefined;
      },
      30_000,
      50,
    );
    await engine.crash();

    // Relaunch on the SAME data dir and port: the boot runs
    // reconcile_orphans before the queue wakes.
    const reborn = await launchEngine({ dataDir, port });
    client = await adminClient(reborn);
    try {
      // The orphan is failed, and the failure names the restart.
      await waitFor(
        "orphan reconciled to failed",
        async () => {
          const job = await client.get(`/api/v1/jobs/${jobId}`);
          return String(job.state ?? job.status ?? "").toLowerCase() === "failed"
            ? job
            : undefined;
        },
        60_000,
        500,
      );
      const job = await client.get(`/api/v1/jobs/${jobId}`);
      const detail = String(
        (job.error as Record<string, unknown> | undefined)?.detail ??
          job.error ??
          job.message ??
          "",
      );
      assert.match(
        detail,
        /restart|orphan/i,
        `the reconciled failure must name the restart, got: ${detail.slice(0, 200)}`,
      );

      // The backup row exists but never sealed.
      const list = await client.get(`/api/v1/instances/${instance.id}/backups`);
      const row = (list.data as Array<Record<string, unknown>>).find(
        (entry) => String(entry.id) === backupId,
      );
      assert.ok(row !== undefined, "the interrupted backup's row survives");
      assert.equal(row?.sha256_hash ?? null, null, "no sha256 without a real archive");

      // The partial archive is gone (cleanup ran — the honest
      // cleanup_pending story, not a promise). Archives live at
      // {data_root}/backups/{instance-short}/{uuid}.tar.gz[.part],
      // so the sweep is recursive.
      const partials = await collectPartials(join(dataDir, "backups"));
      assert.equal(partials.length, 0, `no .part file may survive the reconciling boot: ${partials.join(", ")}`);

      // The pipeline is not poisoned: a fresh backup seals.
      const again = await client.post(`/api/v1/instances/${instance.id}/backups`);
      const againId = String(again.backup_id);
      await waitFor(
        "the follow-up backup seals",
        async () => {
          const listAfter = await client.get(`/api/v1/instances/${instance.id}/backups`);
          const fresh = (listAfter.data as Array<Record<string, unknown>>).find(
            (entry) => String(entry.id) === againId,
          );
          const sealed =
            fresh !== undefined &&
            typeof fresh.sha256_hash === "string" &&
            (fresh.sha256_hash as string).length === 64;
          return sealed ? fresh : undefined;
        },
        180_000,
        1_000,
      );
    } finally {
      await reborn.stop();
    }
  } finally {
    // The first engine already crashed; stop() on a dead handle is
    // the documented no-op.
    await engine.stop();
  }
});
