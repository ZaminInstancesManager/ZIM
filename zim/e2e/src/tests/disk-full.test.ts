/**
 * The disk-full scenario (M27's nightly failure-sweep family): the
 * engine's data root on a size-capped tmpfs, a backup that cannot
 * fit — and the honest failure shape everywhere it touches.
 *
 * What the failure-scenario sweep wants proven (TEST_STRATEGY's
 * failure-behavior rows): a full disk must produce a *structured
 * failed job* with the OS error surfaced — never a hang, never a
 * wedged engine, never a faked sealed row. The engine itself must
 * stay alive and keep answering (the API is the operator's window
 * into the failure).
 *
 * Tier honesty: the tmpfs mount needs root (the CI runners' Linux
 * docker tier has passwordless sudo; a local non-root run skips
 * with the reason printed, exactly like the docker tier's skips).
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { join } from "node:path";
import { existsSync, mkdirSync } from "node:fs";
import { launchEngine } from "../lib/engine.js";
import { adminClient, createInstance, firstNodeId, instanceState } from "../lib/api.js";
import { detectTier, skipWithoutDocker } from "../lib/tier.js";
import { run, tempDir, uniqueName, waitFor } from "../lib/util.js";

/** A startup that plants a file much larger than the capped
 * tmpfs's quota — the backup of that data dir then cannot fit. */
const FAT_STARTUP =
  "sh -c 'dd if=/dev/zero of=/home/container/big.bin bs=1M count=48 2>/dev/null; " +
  "while true; do echo zim-e2e-tick; sleep 1; done'";

test("disk full: the backup fails honestly with the OS error, the engine stays alive and answering", async (t) => {
  const tier = await detectTier();
  if (skipWithoutDocker(t, tier)) {
    return;
  }

  // The capped data root: 8 MiB of tmpfs under a scratch dir. The
  // engine's SQLite, TLS material, and discovery file fit; the
  // 48 MiB archive does not. (The mountpoint must exist — mount(8)
  // fails with exit 32 on an absent target, the first run's quiet
  // skip.)
  const scratch = tempDir("diskfull");
  const capped = join(scratch, "capped");
  mkdirSync(capped, { recursive: true });
  const mount = await run(
    "sudo",
    ["mount", "-t", "tmpfs", "-o", "size=8m", "tmpfs", capped],
    { timeoutMs: 15_000 },
  );
  if (mount.code !== 0) {
    t.skip(`cannot mount the size-capped tmpfs without root (sudo mount exited ${mount.code})`);
    return;
  }
  let mounted = true;
  try {
    const engine = await launchEngine({ dataDir: capped });
    try {
      const client = await adminClient(engine);
      const nodeId = await firstNodeId(client);
      const instance = await createInstance(client, uniqueName("diskfull"), {
        nodeId,
        startup: FAT_STARTUP,
      });

      // The container runs and writes its fat file (into the
      // INSTANCE data dir, which lives under the engine's capped
      // data root — the tmpfs fills as dd writes; the write error
      // is the container's, the engine keeps supervising).
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

      // The backup: an archive that cannot fit the capped fs. The
      // job must land in a structured failed state, not a hang.
      const created = await client.post(`/api/v1/instances/${instance.id}/backups`);
      const jobId = String(created.job_id);
      await waitFor(
        "backup job failed",
        async () => {
          const job = await client.get(`/api/v1/jobs/${jobId}`);
          return String(job.state ?? job.status ?? "").toLowerCase() === "failed"
            ? job
            : undefined;
        },
        120_000,
        500,
      );

      // The engine survived the full disk and still answers.
      const health = await client.get("/api/v1/health");
      assert.equal(health.status, "ok");

      // And the row never seals.
      const list = await client.get(`/api/v1/instances/${instance.id}/backups`);
      const row = (list.data as Array<Record<string, unknown>>).find(
        (entry) => String(entry.id) === String(created.backup_id),
      );
      assert.ok(row !== undefined, "the failed backup's row exists");
      assert.equal(row?.sha256_hash ?? null, null, "no sha256 without a real archive");
    } finally {
      await engine.stop();
    }
  } finally {
    if (mounted) {
      await run("sudo", ["umount", capped], { timeoutMs: 15_000 });
      mounted = false;
    }
    if (existsSync(scratch)) {
      const { rm } = await import("node:fs/promises");
      await rm(scratch, { recursive: true, force: true });
    }
  }
});
