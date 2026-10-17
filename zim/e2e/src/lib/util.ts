/**
 * Shared test utilities: temp dirs, free ports, eventual waits,
 * subprocess helpers. Everything is stdlib-only — the E2E suite's
 * only third-party dependency is Playwright (the browser itself).
 */

import { mkdtempSync, rmSync, readFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createServer } from "node:net";
import { spawn } from "node:child_process";

/** One layer of process.env overlay for spawned processes. */
export type Env = Record<string, string>;

/**
 * Reserve a free loopback port by binding :0 and reading back the
 * assigned port. The listener is closed before returning, so the
 * port is *free* at return — racy in theory, fine in practice for
 * a single-purpose CI host, and the engine retries nothing: a
 * collision fails the boot loudly, which is exactly what we want
 * a test to do.
 */
export async function freePort(): Promise<number> {
  return await new Promise((resolve, reject) => {
    const server = createServer();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (address === null || typeof address === "string") {
        reject(new Error("no port assigned"));
        return;
      }
      const { port } = address;
      server.close(() => resolve(port));
    });
  });
}

/** A unique scratch directory under the OS temp dir. */
export function tempDir(prefix: string): string {
  return mkdtempSync(join(tmpdir(), `zim-e2e-${prefix}-`));
}

/** Remove a directory tree, ignoring "already gone". */
export function cleanupDir(dir: string): void {
  rmSync(dir, { recursive: true, force: true });
}

/** Sleep for `ms` milliseconds. */
export function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * Poll until `probe` returns non-undefined (truthy assertions are
 * the caller's business — `undefined` means "not yet"). Rejects
 * with the last failure after `timeoutMs`.
 */
export async function waitFor<T>(
  what: string,
  probe: () => Promise<T | undefined> | T | undefined,
  timeoutMs: number,
  intervalMs = 250,
): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  let lastError: unknown = undefined;
  while (Date.now() < deadline) {
    try {
      const value = await probe();
      if (value !== undefined) {
        return value;
      }
    } catch (error) {
      lastError = error;
    }
    await sleep(intervalMs);
  }
  const detail =
    lastError === undefined ? "" : ` (last error: ${String(lastError)})`;
  throw new Error(`timed out after ${timeoutMs}ms waiting for ${what}${detail}`);
}

/** Read and parse a JSON file if it exists (undefined otherwise). */
export function readJsonIfPresent(path: string): unknown {
  if (!existsSync(path)) {
    return undefined;
  }
  return JSON.parse(readFileSync(path, "utf8"));
}

/** A lowercase unique-enough token for names inside one run. */
export function uniqueName(base: string): string {
  const stamp = `${Date.now().toString(36)}${Math.floor(Math.random() * 1e6).toString(36)}`;
  return `${base}-${stamp}`;
}

/**
 * Run a command, capture stdout/stderr, and report an exit code.
 * Never rejects: a missing command resolves as `code: 127` (the
 * shell convention), a timeout as `code: 124` after SIGKILL — the
 * callers decide what failures mean (the tier probe treats any
 * non-zero as "not available", which is exactly right for a
 * docker CLI that hangs on daemon-less hosts like the Windows
 * runners).
 */
export async function run(
  command: string,
  args: string[],
  options: { cwd?: string; env?: Env; timeoutMs?: number } = {},
): Promise<{ stdout: string; stderr: string; code: number }> {
  const child = spawn(command, args, {
    cwd: options.cwd,
    env: { ...process.env, ...(options.env ?? {}) },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stdout = "";
  let stderr = "";
  child.stdout.on("data", (chunk: Buffer) => {
    stdout += chunk.toString("utf8");
  });
  child.stderr.on("data", (chunk: Buffer) => {
    stderr += chunk.toString("utf8");
  });
  const timeoutMs = options.timeoutMs ?? 120_000;
  const code = await new Promise<number>((resolve) => {
    const timer = setTimeout(() => {
      child.kill("SIGKILL");
      resolve(124); // timeout, by convention
    }, timeoutMs);
    child.on("error", () => {
      clearTimeout(timer);
      resolve(127); // command not found, by convention
    });
    child.on("close", (exitCode) => {
      clearTimeout(timer);
      resolve(exitCode ?? -1);
    });
  });
  return { stdout, stderr, code };
}

/**
 * The engine binary to drive: `ZIM_E2E_ENGINE` when set, else the
 * repo-relative debug build CI produces — `zim/engine/target/debug/
 * zim_engine[.exe]`, which from `.build/lib` is three levels up
 * (`lib → .build → e2e → zim`) plus the engine tree.
 */
export function engineBinaryPath(): string {
  const override = process.env.ZIM_E2E_ENGINE;
  if (override !== undefined && override !== "") {
    return override;
  }
  const binary = process.platform === "win32" ? "zim_engine.exe" : "zim_engine";
  return join(import.meta.dirname, "..", "..", "..", "engine", "target", "debug", binary);
}

/**
 * The repo root: the compiled file sits at `zim/e2e/.build/lib/`,
 * so the root is four levels up (`lib → .build → e2e → zim → repo`).
 */
export function repoRoot(): string {
  return join(import.meta.dirname, "..", "..", "..", "..");
}
