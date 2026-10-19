/**
 * The §11 event stream, from Node: one WebSocket against the
 * engine's TLS loopback origin, authenticated with the API client's
 * session cookie (the same credential the real UI's panes hold),
 * speaking the `zim-events.v1` sub-protocol.
 *
 * ## Why this exists (M30 CI fix 13)
 *
 * E7's recovering window — `offline` → `starting` — can pass in a
 * few hundred milliseconds under the suite's parallel load, and an
 * HTTP poll can blink straight past it (run 51: sixty seconds of
 * 150 ms polls on ubuntu, not one catching the yellow phase, while
 * the Windows and macOS legs caught it every time). Transitions
 * are EVENTS on the engine's stream — §11.1's
 * `instance.state_change` publishes from `set_observed` itself on
 * every status-vocabulary change — and an event cannot be missed
 * by timing: it queues for the subscriber and arrives in
 * generation order. Observing a transition SEQUENCE through the
 * stream is the deterministic instrument; the HTTP poll remains
 * the right tool for steady end-states (and for everything the
 * stream deliberately does not carry).
 *
 * ## Mechanics
 *
 * Node's native WebSocket is undici's. The WHATWG constructor takes
 * no headers, but undici's extension form —
 * `new WebSocket(url, { protocols, headers })` — does, and the
 * cookie rides the upgrade request exactly as it rides every HTTP
 * call the harness makes. The self-signed loopback certificate is
 * already trusted process-wide (`lib/api.ts` sets
 * `NODE_TLS_REJECT_UNAUTHORIZED=0` before any fetch; undici's
 * WebSocket shares that connector). The engine pings every 30 s
 * and the native client answers pings per the protocol, so an idle
 * recorder holds the connection open without keepalive of its own.
 *
 * The handshake auth is the standard credential resolution (the
 * same path the engine's own `stream_ws` integration tests drive
 * with a cookie), and the authorization is the caller's
 * `WebsocketConnect` grant — the bootstrap admin qualifies.
 */

import type { EngineHandle } from "./engine.js";
import type { ApiClient } from "./api.js";

/** The only sub-protocol this engine speaks (§11). */
const WIRE_SUBPROTOCOL = "zim-events.v1";

/** The undici WebSocket-constructor extension form (headers on the
 * upgrade) — not in the WHATWG-typed surface, so it is asserted
 * here once, at the only place that needs it. */
type HeaderedWebSocketCtor = new (
  url: string,
  options: { protocols: string[]; headers: Record<string, string> },
) => WebSocket;

/** One §11.1 envelope as received (`{type, generation, ts,
 * instance_id, data}`), plus the local receipt time. */
export interface StreamEventRecord {
  type: string;
  generation: number;
  ts: string;
  instance_id: string | null;
  data: Record<string, unknown>;
  /** Local `Date.now()` at receipt (diagnostics: arrival cadence). */
  receivedAt: number;
}

/** One `instance.state_change` transition, flattened for asserts. */
export interface StateChangeRecord {
  /** The status observed before the transition (snake_case wire name). */
  from: string;
  /** The status observed after (snake_case wire name). */
  to: string;
  generation: number;
  receivedAt: number;
}

/** A live §11 subscription collecting events for its lifetime. */
export interface StreamRecorder {
  /** Every event received so far, in arrival order (diagnostics
   * first, assertions second — a failing test prints what the
   * stream actually saw). */
  events(): StreamEventRecord[];
  /** The `instance.state_change` transitions, in arrival order. */
  stateChanges(): StateChangeRecord[];
  /** The engine's cumulative `stream.dropped` count for this
   * subscriber (a non-zero value means the recorder read too slow
   * and the recorded sequence may have holes — assert it zero when
   * completeness matters). */
  droppedEvents(): number;
  /** Close the socket. Idempotent; safe in a `finally`. */
  close(): Promise<void>;
}

/**
 * Open the event stream for one instance and record everything it
 * delivers. Resolves once the WebSocket is OPEN (the subscription
 * is live server-side at upgrade time) — but see
 * [`awaitFirstEvent`]: open is transport-liveness, not
 * engine-liveness.
 */
export async function openEventStream(
  engine: EngineHandle,
  client: ApiClient,
  instanceId: string,
): Promise<StreamRecorder> {
  const url = `${engine.origin.replace(/^https:/, "wss:")}/api/v1/stream?instance_id=${encodeURIComponent(instanceId)}`;

  // The cast is the documented undici extension (see the module
  // docs); the native global's WHATWG types do not carry it.
  const HeaderedWebSocket = WebSocket as unknown as HeaderedWebSocketCtor;
  const socket = new HeaderedWebSocket(url, {
    protocols: [WIRE_SUBPROTOCOL],
    headers: { Cookie: client.sessionCookie() },
  });

  const events: StreamEventRecord[] = [];
  const changes: StateChangeRecord[] = [];
  let dropped = 0;
  let closed = false;

  const recordFrame = (raw: unknown): void => {
    let parsed: Record<string, unknown>;
    try {
      parsed = JSON.parse(String(raw)) as Record<string, unknown>;
    } catch {
      // Not JSON — the v1 contract only sends JSON text frames;
      // keep it as a synthetic record rather than losing it.
      parsed = { type: "(unparseable)", data: { raw: String(raw) } };
    }
    events.push({
      type: String(parsed["type"] ?? "?"),
      generation: Number(parsed["generation"] ?? 0),
      ts: String(parsed["ts"] ?? ""),
      instance_id:
        typeof parsed["instance_id"] === "string" ? (parsed["instance_id"] as string) : null,
      data: (parsed["data"] ?? {}) as Record<string, unknown>,
      receivedAt: Date.now(),
    });
    const last = events[events.length - 1]!;
    if (last.type === "instance.state_change") {
      changes.push({
        from: String(last.data["from"] ?? "?"),
        to: String(last.data["to"] ?? "?"),
        generation: last.generation,
        receivedAt: last.receivedAt,
      });
    } else if (last.type === "stream.dropped") {
      dropped = Number(last.data["dropped_events"] ?? 0);
    }
  };

  socket.onmessage = (message: { data: unknown }) => {
    recordFrame(message.data);
  };

  await new Promise<void>((resolve, reject) => {
    socket.onopen = () => {
      resolve();
    };
    // The WHATWG error event carries no detail of its own; the
    // close event that follows it carries the code and reason.
    socket.onerror = () => {};
    socket.onclose = (event: { code: number; reason: string }) => {
      reject(
        new Error(
          `the stream socket closed before opening (code ${event.code}, reason ${event.reason.slice(0, 200)})`,
        ),
      );
    };
  });
  // Past the handshake, a spontaneous close just means the arrays
  // stop growing (the engine went away with the test's own
  // teardown); nothing here may throw outside close().
  socket.onclose = null;

  return {
    events() {
      return [...events];
    },
    stateChanges() {
      return [...changes];
    },
    droppedEvents() {
      return dropped;
    },
    async close() {
      if (closed) {
        return;
      }
      closed = true;
      socket.onmessage = null;
      socket.onclose = null;
      socket.onerror = null;
      await new Promise<void>((resolve) => {
        socket.onclose = () => resolve();
        try {
          socket.close();
        } catch {
          // already closed — resolve immediately.
          resolve();
        }
        // A socket that ignores close() cannot hang the suite: the
        // engine tears the subscriber down with the connection.
        setTimeout(resolve, 5_000).unref?.();
      });
    },
  };
}

/**
 * Wait until the recorder has received at least one event — the
 * honest liveness gate for an instrument about to carry the whole
 * assertion. An open socket proves transport; the first frame
 * proves the engine is actually delivering THIS instance's lane
 * (a running instance emits `instance.resource_usage` samples on
 * every monitor tick, so the wait is seconds, not minutes). A
 * recorder that never warms up fails the test with "the stream
 * delivered nothing", never with a misleading "no transitions".
 */
export async function awaitFirstEvent(
  recorder: StreamRecorder,
  timeoutMs: number,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (recorder.events().length > 0) {
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  throw new Error(
    `the event stream delivered nothing within ${timeoutMs} ms of opening — ` +
      "the instrument cannot assert transitions over a silent stream " +
      "(engine-side delivery or the subscription filter is broken)",
  );
}
