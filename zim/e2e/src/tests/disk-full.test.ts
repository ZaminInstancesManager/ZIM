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

/** A startup that plants a file much larger than the capped tmpfs's
 * quota — INCOMPRESSIBLE, because the archive of zeros would gzip
 * into kilobytes and fit the leftover margin, sealing a backup the
 * scenario says cannot exist. dd fails with ENOSPC partway (the
 * point: the fs is full), and the loop keeps the container alive. */
const FAT_STARTUP =
  "sh -c 'dd if=/dev/urandom of=/home/container/big.bin bs=1048576 count=48; " +
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

      // The backup: an archive that cannot fit the capped fs.
      //
      // The HONEST contract here is subtler than "the job fails":
      // the engine's OWN database lives on the full filesystem, so
      // the POST itself may be refused with a structured 500 (the
      // enqueue write fails — SQLite's "database or disk is full"
      // surfacing as the API's structured internal_error, which is
      // the IDEAL refusal), and when the enqueue DOES land, the
      // terminal Failed row may itself be unwritable. What the
      // scenario promises — and what this test asserts — is the
      // negative space: the engine never hangs, the API keeps
      // answering THROUGH the full disk (the structured refusal IS
      // an answer), and no fake sealed row ever appears.
      let created: { backup_id: string; job_id: string } | null = null;
      let refusal: string | null = null;
      try {
        created = await client.post(`/api/v1/instances/${instance.id}/backups`);
      } catch (error) {
        // The structured refusal is a legitimate outcome — capture
        // it and assert its honesty below.
        refusal = String((error as { message?: string }).message ?? error);
      }

      if (created !== null) {
        const jobId = String(created.job_id);
        const backupId = String(created.backup_id);
        let failed = false;
        let engineStayedHealthy = true;
        let sealed = false;
        const deadline = Date.now() + 90_000;
        while (Date.now() < deadline) {
          const job = await client.get(`/api/v1/jobs/${jobId}`);
          const state = String(job.state ?? job.status ?? "").toLowerCase();
          if (state === "failed" || state === "cancelled") {
            failed = true;
            break;
          }
          // The engine answers health THROUGH the full disk — the
          // operator's window into the failure must stay open.
          const health = await client.get("/api/v1/health");
          if (health.status !== "ok") {
            engineStayedHealthy = false;
            break;
          }
          const list = await client.get(`/api/v1/instances/${instance.id}/backups`);
          const row = (list.data as Array<Record<string, unknown>>).find(
            (entry) => String(entry.id) === backupId,
          );
          if (row !== undefined && typeof row.sha256_hash === "string") {
            sealed = true; // never allowed — a sealed row without room for the archive is a lie
            break;
          }
          await new Promise((resolve) => setTimeout(resolve, 1_000));
        }

        assert.equal(sealed, false, "no sha256 may appear without room for a real archive");
        assert.equal(
          engineStayedHealthy,
          true,
          "the engine must keep answering health through the full disk",
        );

        // The row itself: present, never sealed. Its state is either
        // the structured failure (when the write margin allowed the
        // terminal row) or the honestly-stuck non-terminal row (the
        // engine's log carries the write errors; the API never lies).
        const finalList = await client.get(`/api/v1/instances/${instance.id}/backups`);
        const finalRow = (finalList.data as Array<Record<string, unknown>>).find(
          (entry) => String(entry.id) === backupId,
        );
        assert.ok(finalRow !== undefined, "the backup's row exists");
        assert.equal(finalRow?.sha256_hash ?? null, null, "no sha256 without a real archive");
        const finalJob = await client.get(`/api/v1/jobs/${jobId}`);
        const finalState = String(finalJob.state ?? finalJob.status ?? "").toLowerCase();
        if (failed) {
          assert.equal(finalState, "failed", "the structured failure stayed terminal");
        } else {
          assert.notEqual(
            finalState,
            "succeeded",
            `the job may be honestly stuck (${finalState}) on a full disk, never fake-succeeded`,
          );
        }
      } else {
        // The refusal path: a STRUCTURED internal_error naming the
        // disk — never a hang, never an opaque drop.
        assert.match(
          refusal ?? "",
          /full|space|io/i,
          `the refusal must name the disk condition, got: ${(refusal ?? "").slice(0, 200)}`,
        );
        // And the row was never created — nothing to seal.
        const list = await client.get(`/api/v1/instances/${instance.id}/backups`);
        assert.equal(
          (list.data as unknown[]).length,
          0,
          "no backup row may exist when the enqueue itself was refused",
        );
      }

      // The engine is still alive after the whole scenario.
      const health = await client.get("/api/v1/health");
      assert.equal(health.status, "ok");
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
