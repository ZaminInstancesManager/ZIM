/**
 * The engine-under-test: spawn the real `zim_engine` binary with a
 * throwaway data dir, learn its port from the discovery file
 * (`engine.json` — the same contract the desktop shell uses), and
 * own its whole lifecycle including the two fault shapes the
 * suite needs: graceful stop (SIGTERM) and crash (SIGKILL).
 *
 * ## Termination discipline (M25 batch 12)
 *
 * The suite's own failure modes taught this file three rules the
 * hard way (a 6-hour CI hang, run 5 of the E2E workflow):
 *
 * 1. **A test process must ALWAYS be able to exit.** An engine the
 *    harness spawned holds its stderr pipe open; if the engine
 *    ignores SIGTERM (a wedged Docker call mid-shutdown), the open
 *    pipe keeps the Node event loop alive forever, the test file's
 *    process never exits, `node --test` never frees its concurrency
 *    slot, and every later test file hangs without ever starting.
 *    So `stop()` escalates SIGTERM → SIGKILL and *destroys the stdio
 *    handles* either way — the parent releases the child's pipes
 *    even when the child refuses to die.
 * 2. **No engine outlives the process that spawned it.** Every
 *    spawned engine is registered process-wide; an `exit` hook
 *    SIGKILLs the survivors (orphan engines would otherwise keep
 *    running containers on the host after a failed test).
 * 3. **A dying engine tells you why it died.** The captured stderr
 *    is printed on stop whenever it looks noisy (start failures,
 *    panics, shutdown problems) — the engine's own "instance X
 *    start failed: …" lines are the single most valuable diagnostic
 *    the suite has, and losing them to a captured-but-never-printed
 *    buffer turned a five-minute diagnosis into a five-hour one.
 */

import { spawn, type ChildProcess } from "node:child_process";
import { join } from "node:path";
import { readFile, rm } from "node:fs/promises";
import {
  engineBinaryPath,
  freePort,
  run,
  sleep,
  tempDir,
  waitFor,
} from "./util.js";

/** The admin credential the harness bootstraps every engine with. */
export interface BootstrapAdmin {
  email: string;
  password: string;
}

export interface EngineHandle {
  /** The scratch data dir (owned by the handle; removed on stop). */
  readonly dataDir: string;
  /** The loopback port the engine serves TLS on. */
  readonly port: number;
  /** The bootstrap admin credential (env-seeded, deterministic). */
  readonly admin: BootstrapAdmin;
  /** `https://127.0.0.1:{port}` — the engine's only origin. */
  readonly origin: string;
  /** SIGTERM (graceful) → SIGKILL (escaltor) + reaped pipes. */
  stop(): Promise<void>;
  /** SIGKILL + wait for exit (the crash-reconciliation shape). */
  crash(): Promise<void>;
  /** The captured stderr tail, for failure diagnostics. */
  stderrTail(lines: number): string;
  /** Print the stderr tail if it looks noisy (diagnostics). */
  reportIfNoisy(): void;
}

/** The discovery file's shape (zim/engine/crates/main/src/discovery.rs). */
interface DiscoveryDoc {
  schema: number;
  pid: number;
  port: number;
}

const ADMIN_EMAIL = "admin@zim.local";

// ── The process-wide engine registry (rule 2) ─────────────────────

const liveEngines = new Set<ChildProcess>();
let exitHookArmed = false;

function registerEngine(child: ChildProcess): void {
  liveEngines.add(child);
  child.on("close", () => {
    liveEngines.delete(child);
  });
  if (!exitHookArmed) {
    exitHookArmed = true;
    process.on("exit", () => {
      for (const engine of liveEngines) {
        try {
          engine.kill("SIGKILL");
        } catch {
          // already gone — the registry just hasn't reaped it yet
        }
      }
    });
  }
}

/**
 * Release every event-loop handle the child holds in THIS process:
 * destroying the piped stdio streams is what actually lets the test
 * process exit even when the child ignores both TERM and KILL
 * (a fully wedged child keeps the loop alive through the socket
 * handle alone — `unref` on the child covers the process handle).
 */
function releasePipes(child: ChildProcess): void {
  for (const stream of [child.stdout, child.stderr]) {
    if (stream !== null && stream !== undefined) {
      stream.destroy();
    }
  }
  try {
    child.unref();
  } catch {
    // already reaped
  }
}

/** Stderr lines worth surfacing on stop (rule 3). */
const NOISY_STDERR = /start failed|panic|shut ?down|error|warn|unavailable/i;

/**
 * Launch a fresh engine. Boot is "healthy" once the discovery file
 * names a port AND `/api/v1/health` answers 200 on it — the same
 * discipline the shell's attach sequence applies.
 */
export async function launchEngine(options: {
  /** Fixed data dir (e.g. a pre-migrated one); a fresh temp dir otherwise. */
  dataDir?: string;
  /** Reuse a reserved port; a free one is picked otherwise. */
  port?: number;
  /** Extra env overlay (rare; the engine's env surface is small). */
  env?: Record<string, string>;
  /** Overall boot timeout. */
  timeoutMs?: number;
}): Promise<EngineHandle> {
  const dataDir = options.dataDir ?? tempDir("engine");
  const port = options.port ?? (await freePort());
  const password = "zim-e2e-admin-password";

  const child = spawn(engineBinaryPath(), ["--data-dir", dataDir, "--port", String(port)], {
    stdio: ["ignore", "ignore", "pipe"],
    env: {
      ...process.env,
      ZIM_BOOTSTRAP_ADMIN_EMAIL: ADMIN_EMAIL,
      ZIM_BOOTSTRAP_ADMIN_PASSWORD: password,
      ...(options.env ?? {}),
    },
  });
  registerEngine(child);

  // Live echo mode: ZIM_E2E_VERBOSE=1 mirrors the engine's stderr
  // onto the console as it happens (the probe's instrument — the
  // engine's own ZIM_DBG trace lines arrive with the diagnosis).
  const verbose = process.env.ZIM_E2E_VERBOSE === "1";
  const stderrLines: string[] = [];
  const tag = `[engine :${port}]`;
  child.stderr.on("data", (chunk: Buffer) => {
    for (const line of chunk.toString("utf8").split("\n")) {
      const trimmed = line.trim();
      if (trimmed !== "") {
        stderrLines.push(trimmed);
        if (verbose) {
          console.error(`${tag} ${trimmed}`);
        }
      }
    }
  });

  const discoveryPath = join(dataDir, "engine.json");
  const timeoutMs = options.timeoutMs ?? 90_000;

  try {
    // 1) engine.json appears with our port (crashed engines leave
    //    it absent or stale — the probe, not the file, decides).
    await waitFor(
      `engine.json to announce port ${port}`,
      async () => {
        let doc: DiscoveryDoc;
        try {
          doc = JSON.parse(await readFile(discoveryPath, "utf8")) as DiscoveryDoc;
        } catch {
          return undefined;
        }
        return doc.port === port ? doc : undefined;
      },
      timeoutMs,
    );

    // 2) Health answers 200 over loopback TLS. TLS validation is
    //    disabled process-wide (util note): the engine's cert is
    //    self-signed for 127.0.0.1 by design.
    await waitFor(
      `health to answer on ${port}`,
      async () => {
        try {
          const response = await fetch(`https://127.0.0.1:${port}/api/v1/health`, {
            signal: AbortSignal.timeout(2_000),
          });
          return response.status === 200 ? response.status : undefined;
        } catch {
          return undefined;
        }
      },
      timeoutMs,
    );
  } catch (error) {
    // Boot failed: reap the child and surface what it printed.
    child.kill("SIGKILL");
    releasePipes(child);
    throw new Error(
      `${String(error)}\nengine stderr tail:\n${stderrLines.slice(-30).join("\n")}`,
    );
  }

  const exited = new Promise<number | null>((resolve) => {
    child.on("close", (code) => resolve(code));
  });

  const handle: EngineHandle = {
    dataDir,
    port,
    origin: `https://127.0.0.1:${port}`,
    admin: { email: ADMIN_EMAIL, password },
    stop: async () => {
      if (child.exitCode !== null || child.signalCode !== null) {
        // Already dead (a prior crash(), or an external kill): just
        // release the pipes so the test process can exit.
        releasePipes(child);
        await rm(dataDir, { recursive: true, force: true }).catch(() => {});
        return;
      }
      child.kill("SIGTERM");
      let code = await withTimeout(exited, 15_000, undefined);
      if (code === TIMEOUT) {
        // The graceful deadline passed: surface what the engine was
        // doing, then escalate — a wedged engine must never wedge
        // the suite (rule 1).
        console.error(
          `${tag} SIGTERM deadline passed — escalating to SIGKILL\n` +
            `${tag} stderr tail:\n${indent(stderrLines.slice(-40))}`,
        );
        child.kill("SIGKILL");
        code = await withTimeout(exited, 10_000, undefined);
        if (code === TIMEOUT) {
          // Even KILL produced no exit event (kernel-level wedge /
          // zombie reaping stall): release the handles regardless.
          console.error(`${tag} engine survived SIGKILL — releasing pipes`);
        }
      }
      releasePipes(child);
      if (typeof code === "number" && code !== 0) {
        console.error(
          `${tag} engine exited ${code} on SIGTERM (expected 0)\n` +
            `${tag} stderr tail:\n${indent(stderrLines.slice(-40))}`,
        );
      }
      await rm(dataDir, { recursive: true, force: true }).catch(() => {});
    },
    crash: async () => {
      child.kill("SIGKILL");
      await withTimeout(exited, 15_000, "engine did not exit after SIGKILL");
      releasePipes(child);
      // Intentionally KEEP the data dir: reconciliation reads it.
    },
    stderrTail: (lines: number) => stderrLines.slice(-lines).join("\n"),
    reportIfNoisy: () => {
      const tail = stderrLines.slice(-60);
      if (tail.some((line) => NOISY_STDERR.test(line))) {
        console.error(`${tag} stderr tail (noisy):\n${indent(tail)}`);
      }
    },
  };
  return handle;
}

function indent(lines: string[]): string {
  return lines.map((line) => `    ${line}`).join("\n");
}

/**
 * Run the offline Pterodactyl importer (`zim_engine migrate
 * pterodactyl --panel-fixture … --zim-db …`) and assert success.
 */
export async function runMigration(options: {
  panelFixture: string;
  zimDb: string;
  dryRun?: boolean;
}): Promise<string> {
  const args = [
    "migrate",
    "pterodactyl",
    `--panel-fixture=${options.panelFixture}`,
    `--zim-db=${options.zimDb}`,
  ];
  if (options.dryRun === true) {
    args.push("--dry-run");
  }
  const result = await run(engineBinaryPath(), args, { timeoutMs: 120_000 });
  if (result.code !== 0) {
    throw new Error(
      `migrate exited ${result.code}\nstdout:\n${result.stdout}\nstderr:\n${result.stderr}`,
    );
  }
  return result.stdout;
}

/**
 * Watch a path until it disappears (crash cleanup assertions).
 * Resolves true when gone, false if still present after `timeoutMs`.
 */
export async function waitGone(path: string, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (!(await fileExists(path))) {
      return true;
    }
    await sleep(100);
  }
  return !(await fileExists(path));
}

async function fileExists(path: string): Promise<boolean> {
  try {
    await readFile(path, "utf8");
    return true;
  } catch {
    return false;
  }
}

/** The sentinel a timed-out race resolves with (see withTimeout). */
const TIMEOUT = Symbol("withTimeout");

function withTimeout<T>(
  promise: Promise<T>,
  timeoutMs: number,
  what: string | undefined,
): Promise<T | typeof TIMEOUT> {
  return Promise.race([
    promise,
    sleep(timeoutMs).then(() => {
      if (what === undefined) {
        return TIMEOUT;
      }
      throw new Error(what);
    }),
  ]) as Promise<T | typeof TIMEOUT>;
}
