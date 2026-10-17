/**
 * The M25 WS probe: WHY does the console page's live stream deliver
 * nothing in the E2E browser? This instrument answers with the
 * browser's OWN network view:
 *
 * - Playwright's `page.on("websocket")` monitors every WS the page
 *   opens — open, close, socketerror, frame counts (the network
 *   truth, independent of the app's own handling);
 * - `page.on("requestfailed")` catches an upgrade that never
 *   completed (TLS/HTTP-level failures land here);
 * - a DIRECT in-page WebSocket (same origin, same cookies, same
 *   sub-protocol) counts frames for 10s — isolating transport from
 *   app logic: if the direct socket receives and the app's doesn't,
 *   the bug is the page's; if neither receives, it's transport or
 *   engine-side;
 * - the console page's own state (view text, generation counter).
 *
 * Usage: `node .build/wsprobe.js` (after `npm run build`). Env:
 *   ZIM_E2E_VERBOSE=1 — echo the engine's stderr live (default on)
 */

process.env.ZIM_E2E_VERBOSE ??= "1";

import { launchEngine, type EngineHandle } from "./lib/engine.js";
import { adminClient, createInstance, firstNodeId, instanceState } from "./lib/api.js";
import { launchBrowser, loginViaUi, newContext, uiGoto } from "./lib/browser.js";
import { detectTier } from "./lib/tier.js";
import { uniqueName, waitFor } from "./lib/util.js";
import type { Page, WebSocket as PwWebSocket } from "playwright";

const T0 = Date.now();
const log = (...parts: unknown[]): void => {
  console.log(`+${((Date.now() - T0) / 1000).toFixed(1)}s`, ...parts);
};
const section = (title: string): void => {
  console.log(`\n${"─".repeat(72)}\n${"─".slice(0, 0)}+${((Date.now() - T0) / 1000).toFixed(1)}s  ${title}\n${"─".repeat(72)}`);
};

interface WsWatch {
  url: string;
  opened: boolean;
  closed: boolean;
  closeCode: number | null;
  socketError: string | null;
  framesReceived: number;
  framesSent: number;
  firstFrameAt: number | null;
}

function watchPageNetwork(page: Page, watches: WsWatch[]): void {
  page.on("websocket", (ws: PwWebSocket) => {
    const watch: WsWatch = {
      url: ws.url(),
      opened: true,
      closed: false,
      closeCode: null,
      socketError: null,
      framesReceived: 0,
      framesSent: 0,
      firstFrameAt: null,
    };
    watches.push(watch);
    log(`  [net] WS opened: ${ws.url()}`);
    ws.on("framereceived", () => {
      watch.framesReceived += 1;
      if (watch.firstFrameAt === null) {
        watch.firstFrameAt = Date.now();
        log(`  [net] first frame received on ${watch.url.slice(0, 80)}`);
      }
    });
    ws.on("framesent", () => {
      watch.framesSent += 1;
    });
    ws.on("socketerror", (error: string) => {
      watch.socketError = String(error);
      log(`  [net] WS socketerror: ${String(error).slice(0, 200)}`);
    });
    ws.on("close", () => {
      watch.closed = true;
      log(`  [net] WS closed: ${watch.url.slice(0, 80)}`);
    });
  });
  page.on("requestfailed", (request) => {
    const failure = request.failure()?.errorText ?? "?";
    log(`  [net] request FAILED: ${request.method()} ${request.url().slice(0, 90)} — ${failure}`);
  });
  page.on("console", (message) => {
    if (message.type() === "error" || message.type() === "warning") {
      log(`  [page ${message.type()}] ${message.text().slice(0, 200)}`);
    }
  });
}

async function main(): Promise<number> {
  let engine: EngineHandle | undefined;
  try {
    section("0. environment");
    const tier = await detectTier();
    log("tier:", JSON.stringify(tier));

    section("1. engine + running instance");
    engine = await launchEngine({});
    const client = await adminClient(engine);
    const nodeId = await firstNodeId(client);
    const instance = await createInstance(client, uniqueName("wsprobe"), { nodeId });
    await client.post(`/api/v1/instances/${instance.id}/start`);
    await waitFor(
      "running state",
      async () => {
        const state = await instanceState(client, instance.id);
        return state.status === "running" ? state : undefined;
      },
      120_000,
      2_000,
    );
    log("instance running:", instance.id);

    section("2. the console page under network watch");
    const browser = await launchBrowser();
    try {
      const context = await newContext(browser);
      const page = await context.newPage();
      const watches: WsWatch[] = [];
      watchPageNetwork(page, watches);

      await loginViaUi(page, engine);
      await uiGoto(page, engine, `servers/${instance.id}/console`);
      log("console page loaded; watching 20s of network activity…");
      await new Promise((resolve) => setTimeout(resolve, 20_000));

      log(`watched ${watches.length} websocket(s):`);
      for (const watch of watches) {
        log(
          `  url=${watch.url.slice(0, 100)}`,
        );
        log(
          `    opened=${watch.opened} closed=${watch.closed} framesReceived=${watch.framesReceived} framesSent=${watch.framesSent} socketError=${JSON.stringify(watch.socketError)}`,
        );
      }

      const meta = await page.locator("#console-meta").textContent().catch(() => null);
      const viewText = await page
        .locator("#console")
        .evaluate((element: { shadowRoot?: { textContent?: string | null } }) => {
          return element.shadowRoot?.textContent ?? "";
        })
        .catch(() => "");
      const tickCount = viewText.split("zim-e2e-tick").length - 1;
      log(`page state: #console-meta=${JSON.stringify(meta)}; ticks rendered=${tickCount}`);

      section("3. the DIRECT in-page WebSocket (transport vs app logic)");
      const direct = await page.evaluate(
        (instanceId: string) =>
          new Promise<Record<string, unknown>>((resolve) => {
            // Structural browser globals (the e2e tsconfig carries no
            // DOM lib by design; the callback runs in the page).
            const g = globalThis as unknown as {
              location: { host: string };
              setTimeout: (fn: () => void, ms: number) => void;
              WebSocket: new (
                url: string,
                protocols: string[],
              ) => {
                onopen: (() => void) | null;
                onmessage: ((message: { data: unknown }) => void) | null;
                onerror: (() => void) | null;
                onclose: ((event: { code: number; reason: string }) => void) | null;
                close(): void;
              };
            };
            const started = Date.now();
            const socket = new g.WebSocket(
              `wss://${g.location.host}/api/v1/stream?instance_id=${encodeURIComponent(instanceId)}`,
              ["zim-events.v1"],
            );
            const report: Record<string, unknown> = {
              opened: false,
              frames: 0,
              firstTypes: [] as string[],
              closeCode: null as number | null,
              closeReason: "",
              errorName: null as string | null,
              waitedMs: 0,
            };
            socket.onopen = () => {
              report.opened = true;
            };
            socket.onmessage = (message: { data: unknown }) => {
              report.frames = (report.frames as number) + 1;
              const types = report.firstTypes as string[];
              if (types.length < 5) {
                try {
                  types.push(String(JSON.parse(String(message.data)).type));
                } catch {
                  types.push("(unparseable)");
                }
              }
            };
            socket.onerror = () => {
              report.errorName = "error event";
            };
            socket.onclose = (event: { code: number; reason: string }) => {
              report.closeCode = event.code;
              report.closeReason = event.reason;
            };
            g.setTimeout(() => {
              report.waitedMs = Date.now() - started;
              try {
                socket.close();
              } catch {
                // already closed
              }
              resolve(report);
            }, 10_000);
          }),
        instance.id,
      );
      log("direct WS report:", JSON.stringify(direct));

      section("4. verdict");
      const appStream = watches.find((watch) => watch.url.includes("/api/v1/stream"));
      const appFrames = appStream?.framesReceived ?? 0;
      const directFrames =
        Number(direct.frames ?? 0);
      if (appFrames > 0 && tickCount > 0) {
        log("APP STREAM LIVE: frames arrived and ticks rendered.");
        return 0;
      }
      if (directFrames > 0 && appFrames === 0) {
        log(
          "TRANSPORT OK, APP BROKEN: the direct socket receives frames but the page's own stream does not — the bug is in the page's stream wiring.",
        );
        return 1;
      }
      if (directFrames === 0) {
        log(
          "TRANSPORT BROKEN: even the direct in-page socket receives nothing (engine-side or upgrade-level).",
        );
        return 2;
      }
      log("PARTIAL: frames arrived but no ticks rendered — page rendering issue.");
      return 3;
    } finally {
      await browser.close();
    }
  } catch (error) {
    section("wsprobe aborted");
    log("!!", error);
    if (engine !== undefined) {
      log("engine stderr tail:\n", engine.stderrTail(60));
      await engine.stop().catch(() => {});
    }
    return 1;
  } finally {
    if (engine !== undefined) {
      engine.reportIfNoisy();
      await engine.stop().catch(() => {});
    }
  }
}

const watchdog = setTimeout(() => {
  console.error("!! WSPROBE WATCHDOG fired");
  process.exit(124);
}, 8 * 60_000);
watchdog.unref?.();

main().then((code) => {
  clearTimeout(watchdog);
  process.exit(code);
});
