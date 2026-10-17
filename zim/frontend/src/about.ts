/**
 * `zim://about` — the About surface (M27, UPDATES.md §3's promised
 * "both versions side by side").
 *
 * Three version numbers, each with a distinct meaning:
 *
 * - **App** — the desktop shell's build version (the bundle's:
 *   shell + engine bytes ship together). Only knowable inside the
 *   desktop app (`window.__TAURI__`, the withGlobalTauri bridge);
 *   a plain browser view shows the honest placeholder instead of
 *   pretending.
 * - **Running engine** — the process actually supervising servers
 *   right now, straight from `GET /api/v1/health`'s `version`
 *   (written from the engine's own `env!`, so it cannot drift).
 * - **Installed engine** — the sidecar binary's `--version` answer:
 *   the bytes that run on the next engine restart. Right after an
 *   update this can LAG the running engine (UPDATES.md §3's
 *   granularity model: the engine keeps running through an update
 *   and the new bytes take over at its next restart) — the page
 *   says exactly that when the two disagree.
 *
 * The update channel's check button rides along (the same
 * consent-shaped cycle the tray item runs; the tray menu stays the
 * flash surface for label-state).
 */

import { health } from "./api";
import { initThemeFromStorage } from "./theme";

initThemeFromStorage();

interface TauriBridge {
  invoke(command: string): Promise<unknown>;
}

/** The withGlobalTauri bridge, when running inside the desktop
 * shell (`undefined` in a plain browser — E2E's browser tier,
 * engine-served views opened directly). */
function tauri(): TauriBridge | null {
  const candidate = (window as unknown as Record<string, unknown>).__TAURI__;
  if (candidate === null || typeof candidate !== "object") {
    return null;
  }
  const invoke = (candidate as Record<string, unknown>).invoke;
  if (typeof invoke !== "function") {
    return null;
  }
  return { invoke: invoke as (command: string) => Promise<unknown> };
}

function text(id: string, value: string): void {
  const node = document.getElementById(id);
  if (node !== null) {
    node.textContent = value;
  }
}

async function run(): Promise<void> {
  // The engine half is always knowable — this page is served BY the
  // engine.
  try {
    const engine = await health();
    text("engine-running", engine.version ?? "unknown (pre-M27 engine)");
    text("engine-api", engine.api_version);
  } catch (error) {
    text("engine-running", `unreachable — ${String(error)}`);
  }

  const bridge = tauri();
  if (bridge === null) {
    text("app-version", "— (shown inside the desktop app)");
    text("engine-installed", "— (shown inside the desktop app)");
    return;
  }

  // The desktop shell's three contributions.
  try {
    const version = (await bridge.invoke("zim_shell_version")) as string;
    text("app-version", `v${version}`);
  } catch (error) {
    text("app-version", `unknown — ${String(error)}`);
  }
  try {
    const installed = (await bridge.invoke("zim_installed_engine_version")) as string | null;
    text("engine-installed", installed ?? "sidecar binary not found");
  } catch (error) {
    text("engine-installed", `unknown — ${String(error)}`);
  }

  // The version-lag notice (UPDATES.md §3): the honest sentence
  // exactly when it is true.
  const lagNote = document.getElementById("version-lag");
  if (lagNote !== null) {
    const running = document.getElementById("engine-running")?.textContent ?? "";
    const installed = document.getElementById("engine-installed")?.textContent ?? "";
    if (
      !running.startsWith("unreachable") &&
      !installed.startsWith("—") &&
      running !== installed
    ) {
      lagNote.hidden = false;
      text(
        "version-lag-detail",
        `The installed engine (${installed}) and the running engine (${running}) differ — ` +
          "the engine keeps running through app updates; the new bytes take over at its " +
          "next restart.",
      );
    }
  }
}

document.getElementById("check-updates")?.addEventListener("click", () => {
  const bridge = tauri();
  const note = document.getElementById("update-note");
  if (bridge === null) {
    if (note !== null) {
      note.textContent = "Update checks run inside the desktop app (the tray has one too).";
    }
    return;
  }
  if (note !== null) {
    note.textContent = "Checking… (progress shows in the tray menu's own label)";
  }
  void bridge
    .invoke("zim_check_updates")
    .catch((error: unknown) => {
      if (note !== null) {
        note.textContent = `Update check failed — ${String(error)}`;
      }
    });
});

void run();
