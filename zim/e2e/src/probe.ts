/**
 * The M25 diagnostic probe: the full §1.4 scenario (create → start →
 * console → stop → backup → schedule run-now) driven API-only, with
 * every step's raw response printed and every failure followed by a
 * full environment dump — the engine's own stderr (echoed LIVE with
 * `ZIM_DBG=1` so the supervision trace lines arrive as they happen)
 * plus the Docker daemon's view of the world.
 *
 * Why this exists alongside the test suite: `node --test` buffers
 * failure detail until the run's summary, and a hung file means the
 * summary never arrives — the probe prints everything as it happens
 * and always terminates (every wait is deadline-bounded). One probe
 * run answers "why does the docker tier fail on CI" with data
 * instead of inference.
 *
 * Usage: `npm run probe` (in zim/e2e). Env:
 *   ZIM_E2E_VERBOSE=1  — echo the engine's stderr live (default on)
 *   ZIM_DBG=1          — the engine's own supervision trace
 *   ZIM_E2E_TIER=…     — force a tier (the harness's knob)
 */

process.env.ZIM_E2E_VERBOSE ??= "1";

import { launchEngine, type EngineHandle } from "./lib/engine.js";
import { adminClient, createInstance, firstNodeId } from "./lib/api.js";
import { detectTier } from "./lib/tier.js";
import { run, uniqueName, waitFor } from "./lib/util.js";

const T0 = Date.now();

function stamp(): string {
  return `+${((Date.now() - T0) / 1000).toFixed(1)}s`;
}

function log(...parts: unknown[]): void {
  console.log(stamp(), ...parts);
}

function section(title: string): void {
  console.log(`\n${"─".repeat(72)}\n${stamp()}  ${title}\n${"─".repeat(72)}`);
}

async function main(): Promise<number> {
  let engine: EngineHandle | undefined;
  try {
    // ── 0) The environment, honestly reported ────────────────────
    section("0. environment");
    const tier = await detectTier();
    log("platform:", process.platform, "| node:", process.versions.node);
    log("tier:", JSON.stringify(tier));
    if (tier.tier === "docker") {
      const info = await run("docker", ["version", "--format", "{{json .}}"], { timeoutMs: 15_000 });
      log("docker version:", info.code === 0 ? info.stdout.trim().slice(0, 400) : `exit ${info.code}: ${info.stderr.slice(0, 200)}`);
      const images = await run("docker", ["images", "--format", "{{.Repository}}:{{.Tag}}"], { timeoutMs: 15_000 });
      log("docker images:", images.stdout.split("\n").filter(Boolean).join(", ").slice(0, 300));
    }

    // ── 1) Engine boot (with the engine's own trace live) ────────
    section("1. engine boot");
    engine = await launchEngine({ env: { ZIM_DBG: "1" } });
    log(`engine up: origin=${engine.origin} dataDir=${engine.dataDir}`);

    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    log("login ok; local node:", nodeId);

    // ── 2) Instance create (allocations first, like the suite) ──
    section("2. instance create");
    const instance = await createInstance(client, uniqueName("probe"), { nodeId });
    log("created:", JSON.stringify(instance));
    const state0 = await client.get(`/api/v1/instances/${instance.id}/state`);
    log("initial state:", JSON.stringify(state0));

    // ── 3) Start → running convergence ──────────────────────────
    section("3. start → running");
    const started = await client.tryPost(`/api/v1/instances/${instance.id}/start`);
    log("POST /start →", started.status, started.body.slice(0, 200) || "(empty body)");
    let running = false;
    try {
      const state = await waitFor(
        "running state",
        async () => {
          const raw = await client.get(`/api/v1/instances/${instance.id}/state`);
          return raw.status === "running" ? raw : undefined;
        },
        120_000,
        2_000,
      );
      running = true;
      log("RUNNING:", JSON.stringify(state));
    } catch (error) {
      log("!! never reached running:", String(error));
      const raw = await client.get(`/api/v1/instances/${instance.id}/state`);
      log("final state:", JSON.stringify(raw));
    }

    // ── 4) Console buffer → live ticks ──────────────────────────
    section("4. console buffer");
    let ticks = 0;
    try {
      const buffer = await waitFor(
        "console output with a probe tick",
        async () => {
          const raw = (await client.get(`/api/v1/instances/${instance.id}/console`)) as {
            events: Array<{ type: string; data?: { line?: string } }>;
            last_generation: number;
          };
          const lines = (raw.events ?? [])
            .filter((event) => event.type === "instance.console_output")
            .map((event) => event.data?.line ?? "");
          const found = lines.filter((line) => line.includes("zim-e2e-tick"));
          if (found.length > 0) {
            return { raw, lines, found };
          }
          return undefined;
        },
        running ? 60_000 : 15_000,
        2_000,
      );
      ticks = buffer.found.length;
      log(`CONSOLE OK: ${buffer.lines.length} line(s) buffered, ${ticks} tick(s)`);
      log("last lines:", JSON.stringify(buffer.lines.slice(-3)));
      log("last_generation:", buffer.raw.last_generation);
    } catch (error) {
      log("!! no console output:", String(error));
      const raw = await client.get(`/api/v1/instances/${instance.id}/console`);
      log("console endpoint raw:", JSON.stringify(raw).slice(0, 500));
    }

    // ── 5) Stop → offline convergence ───────────────────────────
    section("5. stop → offline");
    if (running) {
      const stopped = await client.tryPost(`/api/v1/instances/${instance.id}/stop`);
      log("POST /stop →", stopped.status, stopped.body.slice(0, 200) || "(empty body)");
      try {
        const state = await waitFor(
          "stopped state",
          async () => {
            const raw = await client.get(`/api/v1/instances/${instance.id}/state`);
            return raw.status === "stopped" || raw.status === "offline" ? raw : undefined;
          },
          90_000,
          2_000,
        );
        log("STOPPED:", JSON.stringify(state));
      } catch (error) {
        log("!! never reached stopped:", String(error));
        const raw = await client.get(`/api/v1/instances/${instance.id}/state`);
        log("final state:", JSON.stringify(raw));
      }
    } else {
      log("skipped (never running)");
    }

    // ── 6) Backup create → sealed row (the jobs pipeline) ───────
    section("6. backup create → seal");
    if (running) {
      const created = await client.post(`/api/v1/instances/${instance.id}/backups`);
      const backupId = String(created.backup_id);
      const jobId = String(created.job_id);
      log("created:", JSON.stringify(created));
      try {
        const sealed = await waitFor(
          "backup sealed (sha256 + bytes)",
          async () => {
            const list = await client.get(`/api/v1/instances/${instance.id}/backups`);
            const row = (list.data as Array<Record<string, unknown>>).find(
              (entry) => String(entry.id) === backupId,
            );
            if (row === undefined) {
              return undefined;
            }
            const ok =
              typeof row.sha256_hash === "string" && row.sha256_hash.length === 64 &&
              typeof row.bytes === "number" && row.bytes > 0;
            return ok ? row : undefined;
          },
          90_000,
          2_000,
        );
        log("SEALED:", JSON.stringify(sealed));
        void jobId;
      } catch (error) {
        log("!! backup never sealed:", String(error));
        const job = await client.get(`/api/v1/jobs/${jobId}`);
        log("job raw:", JSON.stringify(job));
        const list = await client.get(`/api/v1/instances/${instance.id}/backups`);
        log("backup row raw:", JSON.stringify(list).slice(0, 400));
      }
    } else {
      log("skipped (never running)");
    }

    // ── 7) Schedule create → run-now (the dispatch contract) ────
    section("7. schedule run-now");
    const schedule = await client.post(`/api/v1/instances/${instance.id}/schedules`, {
      name: "probe restart",
      cron: "0 4 * * *",
      is_active: true,
      only_when_online: false,
      actions: [{ type: "power", action: "restart" }],
    });
    log("created schedule:", JSON.stringify(schedule).slice(0, 200));
    const triggered = await client.tryPost(
      `/api/v1/instances/${instance.id}/schedules/${schedule.id}/run`,
    );
    log(
      "POST /run →", triggered.status, triggered.body.slice(0, 300) || "(empty body — the 204 contract)",
    );

    // ── 8) Graceful engine shutdown (the hang detector) ─────────
    section("8. engine shutdown");
    const shutdownStart = Date.now();
    await engine.stop();
    log(`engine stopped in ${((Date.now() - shutdownStart) / 1000).toFixed(1)}s (SIGTERM path)`);

    // ── Verdict ─────────────────────────────────────────────────
    section("verdict");
    const failures: string[] = [];
    if (!running) {
      failures.push("instance never reached running");
    }
    if (running && ticks === 0) {
      failures.push("console buffer never collected a tick");
    }
    if (failures.length === 0) {
      log("PROBE GREEN: the full §1.4 API scenario works on this host.");
      return 0;
    }
    log("PROBE RED:", failures.join("; "));
    return 1;
  } catch (error) {
    section("probe aborted");
    log("!!", error);
    if (engine !== undefined) {
      log("engine stderr tail:\n", engine.stderrTail(80));
      await engine.stop().catch(() => {});
    }
    await dumpDocker();
    return 1;
  } finally {
    if (engine !== undefined) {
      engine.reportIfNoisy();
    }
  }
}

/** The Docker daemon's view: containers, network, logs, states. */
async function dumpDocker(): Promise<void> {
  section("docker diagnostics");
  const tier = await detectTier();
  if (tier.tier !== "docker") {
    log("(null tier — nothing to dump)");
    return;
  }
  const ps = await run("docker", ["ps", "-a", "--filter", "label=Service=ZIM", "--format", "{{.ID}} {{.Names}} {{.Status}}"], { timeoutMs: 15_000 });
  log("zim containers:", ps.code === 0 ? ps.stdout.trim() || "(none)" : `exit ${ps.code}`);
  const networks = await run("docker", ["network", "ls", "--format", "{{.Name}}"], { timeoutMs: 15_000 });
  log("networks:", networks.stdout.split("\n").filter(Boolean).join(", "));
  const netInspect = await run("docker", ["network", "inspect", "pterodactyl0"], { timeoutMs: 15_000 });
  log("pterodactyl0:", netInspect.code === 0 ? netInspect.stdout.slice(0, 300) : `exit ${netInspect.code}`);
  const ids = ps.stdout.split("\n").filter(Boolean).map((line) => line.split(" ")[0]);
  for (const id of ids.slice(0, 6)) {
    const inspect = await run("docker", ["inspect", id, "--format", "{{json .State}} | {{json .HostConfig.Binds}} | {{json .Config.Cmd}}"], { timeoutMs: 15_000 });
    log(`container ${id}:`, inspect.stdout.trim().slice(0, 400));
    const logs = await run("docker", ["logs", "--tail", "20", id], { timeoutMs: 15_000 });
    log(`logs ${id}:`, (logs.stdout + logs.stderr).trim().slice(0, 400) || "(empty)");
  }
}

// The probe always terminates: a global watchdog hard-exits 9 minutes
// in no matter what wedged (the suite's own discipline, rule 1).
const watchdog = setTimeout(() => {
  console.error(`\n${stamp()} !! PROBE WATCHDOG fired — aborting`);
  process.exit(124);
}, 9 * 60_000);
watchdog.unref?.();

main()
  .then((code) => {
    clearTimeout(watchdog);
    process.exit(code);
  })
  .catch((error) => {
    clearTimeout(watchdog);
    console.error(stamp(), "!! unhandled:", error);
    process.exit(1);
  });
