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
    timeoutMs: 20_000,
  });
  if (probe.code === 0 && probe.stdout.trim() !== "") {
    cached = { tier: "docker", detail: `docker server ${probe.stdout.trim()}` };
  } else {
    cached = {
      tier: "null",
      detail: "no Docker daemon answered `docker version`",
    };
  }
  return cached;
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
