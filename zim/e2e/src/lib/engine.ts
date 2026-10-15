/**
 * The engine-under-test: spawn the real `zim_engine` binary with a
 * throwaway data dir, learn its port from the discovery file
 * (`engine.json` — the same contract the desktop shell uses), and
 * own its whole lifecycle including the two fault shapes the
 * suite needs: graceful stop (SIGTERM) and crash (SIGKILL).
 */

import { spawn } from "node:child_process";
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
  /** SIGTERM + wait for exit (the graceful path removes engine.json). */
  stop(): Promise<void>;
  /** SIGKILL + wait for exit (the crash-reconciliation shape). */
  crash(): Promise<void>;
  /** The captured stderr tail, for failure diagnostics. */
  stderrTail(lines: number): string;
}

/** The discovery file's shape (zim/engine/crates/main/src/discovery.rs). */
interface DiscoveryDoc {
  schema: number;
  pid: number;
  port: number;
}

const ADMIN_EMAIL = "admin@zim.local";

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

  const stderrLines: string[] = [];
  child.stderr.on("data", (chunk: Buffer) => {
    for (const line of chunk.toString("utf8").split("\n")) {
      const trimmed = line.trim();
      if (trimmed !== "") {
        stderrLines.push(trimmed);
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
      child.kill("SIGTERM");
      const code = await withTimeout(exited, 15_000, "engine did not exit after SIGTERM");
      if (code !== null && code !== 0) {
        throw new Error(`engine exited ${code} on SIGTERM (expected 0)\n${handle.stderrTail(30)}`);
      }
      await rm(dataDir, { recursive: true, force: true });
    },
    crash: async () => {
      child.kill("SIGKILL");
      await withTimeout(exited, 15_000, "engine did not exit after SIGKILL");
      // Intentionally KEEP the data dir: reconciliation reads it.
    },
    stderrTail: (lines: number) => stderrLines.slice(-lines).join("\n"),
  };
  return handle;
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

function withTimeout<T>(
  promise: Promise<T>,
  timeoutMs: number,
  what: string,
): Promise<T> {
  return Promise.race([
    promise,
    sleep(timeoutMs).then(() => {
      throw new Error(what);
    }),
  ]);
}
