/**
 * The runtime tier model (AD-30): a Docker daemon that answers
 * `docker version` makes the suite's lifecycle tests real; without
 * one the engine itself boots in degraded API-only mode
 * (NullRuntime), and the lifecycle tests skip — honestly, with the
 * reason printed — while everything else (boot, login, UI,
 * sessions, extensions, files, backups, schedules, themes,
 * migration, fault injection) runs for real.
 */

import { run } from "./util.js";

import { existsSync } from "node:fs";

export type Tier = "docker" | "null";

/** The tier's provenance, for skip messages and diagnostics. */
export interface TierInfo {
  readonly tier: Tier;
  readonly detail: string;
}

let cached: TierInfo | undefined;

/** Detect the tier once per process (env override wins). */
export async function detectTier(): Promise<TierInfo> {
  if (cached !== undefined) {
    return cached;
  }
  const override = process.env.ZIM_E2E_TIER;
  if (override === "docker" || override === "null") {
    cached = { tier: override, detail: "forced via ZIM_E2E_TIER" };
    return cached;
  }
  const probe = await run("docker", ["version", "--format", "{{.Server.Version}}"], {
    // Short on purpose: daemon-less hosts with a docker CLI stub
    // (the Windows runners) HANG here — a timeout is a null-tier
    // answer, not a suite failure.
    timeoutMs: 10_000,
  });
  if (probe.code === 0 && probe.stdout.trim() !== "" && engineEndpointPresent()) {
    cached = { tier: "docker", detail: `docker server ${probe.stdout.trim()}` };
  } else {
    cached = {
      tier: "null",
      detail:
        probe.code === 124
          ? "the docker CLI hung (no daemon answered within 10s)"
          : probe.code === 127
            ? "no docker CLI on PATH"
            : probe.code !== 0
              ? `docker version exited ${probe.code}`
              : "docker answers the CLI, but the engine's local endpoint (the unix socket / named pipe bollard's local defaults use) is absent",
    };
  }
  return cached;
}

/**
 * The endpoint the ENGINE's DockerRuntimeFactory actually uses:
 * bollard's local defaults — `/var/run/docker.sock` off Windows,
 * the `docker_engine` named pipe on it. A CLI that answers over
 * TCP (GitHub's Windows runners set DOCKER_HOST) is NOT a tier the
 * engine can use, and pretending otherwise just fails every
 * lifecycle test with 503s.
 */
function engineEndpointPresent(): boolean {
  if (process.platform === "win32") {
    return existsSync("\\\\.\\pipe\\docker_engine");
  }
  return existsSync("/var/run/docker.sock");
}

/**
 * The honest skip: node:test's `t.skip()` prints the reason in the
 * run output, keeping the verification discipline (§7 of the test
 * strategy) intact — skipped is never silently green.
 */
export function skipWithoutDocker(t: {
  skip(message?: string): unknown;
}, info: TierInfo): boolean {
  if (info.tier === "docker") {
    return false;
  }
  t.skip(`docker tier only — ${info.detail}`);
  return true;
}
