/**
 * The ZIM addon SDK client (`zim.js`, M24).
 *
 * One vendored, dependency-free script an addon page loads from its
 * own package (`<script src="/zim/ext/{addon-id}/assets/zim.js">`)
 * that speaks the engine's loopback API (`/api/v1`, same origin,
 * `zim_session` cookie credentials) and the §11 event stream.
 *
 * What it provides:
 *
 * - `Zim.init(addonId)` — resolve this addon's registration (the
 *   granted permission set, the enabled flag) from
 *   `GET /api/v1/extensions`. Called once at boot; returns the
 *   addon's summary.
 * - `Zim.api(path, options)` — the §6 fetch wrapper (same-origin,
 *   cookie credentials, structured error parsing) with an optional
 *   `requiredPermission` gate checked client-side against the
 *   granted set before any request leaves the page.
 * - `Zim.assertPermission(name)` — the same gate, standalone.
 * - `Zim.openInstanceStream(instanceId, handlers)` — one §11
 *   WebSocket subscription per instance (sub-protocol
 *   `zim-events.v1`, exponential backoff, replay-from-checkpoint).
 * - `Zim.whoami()` — the signed-in user (null when signed out).
 *
 * The client-side permission gate is UX, not security: the engine's
 * per-endpoint authz (invariant 15) gates every call regardless.
 * Both layers agree in practice; when they do not, the engine wins.
 *
 * This file is authored as a plain script (no module imports): the
 * compiled `dist/zim.js` is vendored verbatim into addon packages.
 */

/** One structured engine error (ZIM_API_SPEC.md §6). */
class ApiError extends Error {
  readonly code: string;
  readonly status: number;
  readonly field: string | undefined;
  readonly missingPermissions: string[];
  readonly requestId: string | undefined;

  constructor(
    status: number,
    code: string,
    message: string,
    field?: string,
    requestId?: string,
    missingPermissions: string[] = [],
  ) {
    super(message);
    this.name = "ZimApiError";
    this.status = status;
    this.code = code;
    this.field = field;
    this.requestId = requestId;
    this.missingPermissions = missingPermissions;
  }

  get isAuth(): boolean {
    return this.status === 401;
  }

  get isPermission(): boolean {
    return this.status === 403;
  }
}

interface ApiOptions {
  method?: string;
  body?: unknown;
  /** A permission grant the addon must hold for this call. */
  requiredPermission?: string;
  query?: Record<string, string | number | undefined>;
}

interface SelfSummary {
  id: string;
  version: string;
  name: string;
  enabled: boolean;
  bundled: boolean;
  instance_scope: string;
  permissions: { requested: string[]; granted: string[] };
  page_surfaces: string[];
  event_subscriptions: string[];
}

interface ListEnvelope<T> {
  data: T[];
  meta?: Record<string, unknown>;
}

interface Me {
  id: number;
  email: string;
  username: string;
  is_admin: boolean;
  root_admin: boolean;
}

type StreamStatus = "connecting" | "live" | "reconnecting" | "down";

interface StreamEvent {
  type: string;
  generation: number;
  data: Record<string, unknown>;
}

interface StreamHandlers {
  onEvent: (event: StreamEvent) => void;
  onGeneration?: (generation: number) => void;
  onStatus?: (status: StreamStatus) => void;
  onGap?: (from: number, to: number) => void;
  onFatal?: (reason: string) => void;
}

const BACKOFF_FLOOR_MS = 1_000;
const BACKOFF_CAP_MS = 15_000;
const MAX_ATTEMPTS = 6;

/** The resolved registration of the addon this script serves. */
let resolvedSelf: SelfSummary | null = null;
let initPromise: Promise<SelfSummary | null> | null = null;
/** This page's addon id, captured at Zim.init (AD-29's attribution:
 * rides every API call as X-ZIM-Addon so the engine's audit log can
 * attribute the action to the addon whose page made it — advisory
 * by design; the user's own credentials still authorize the call). */
let attributedAddonId: string | null = null;

async function parseError(response: Response): Promise<ApiError> {
  let code = "network_error";
  let message = `${response.status} ${response.statusText || "(no status text)"}`;
  let field: string | undefined;
  let requestId: string | undefined;
  let missing: string[] = [];
  try {
    const body = (await response.json()) as { error?: Record<string, unknown> };
    const err = body && body.error;
    if (err) {
      if (typeof err.code === "string") code = err.code;
      if (typeof err.message === "string") message = err.message;
      if (typeof err.field === "string") field = err.field;
      if (typeof err.request_id === "string") requestId = err.request_id;
      if (Array.isArray(err.missing_permissions)) {
        missing = err.missing_permissions.filter((p): p is string => typeof p === "string");
      }
    }
  } catch {
    // Body was not JSON — keep the HTTP-level fallback text.
  }
  return new ApiError(response.status, code, message, field, requestId, missing);
}

function assertPermission(name: string): void {
  if (resolvedSelf === null) {
    throw new ApiError(0, "not_initialized", "Zim.init(addonId) must resolve before gated calls");
  }
  if (!resolvedSelf.permissions.granted.includes(name)) {
    throw new ApiError(
      403,
      "permission_denied",
      `this addon was not granted ${JSON.stringify(name)}`,
      undefined,
      undefined,
      [name],
    );
  }
}

async function api<T = unknown>(path: string, options: ApiOptions = {}): Promise<T> {
  if (options.requiredPermission !== undefined) {
    assertPermission(options.requiredPermission);
  }
  let url = path;
  if (options.query !== undefined) {
    const params = new URLSearchParams();
    for (const [key, value] of Object.entries(options.query)) {
      if (value !== undefined) {
        params.set(key, String(value));
      }
    }
    const suffix = params.toString();
    if (suffix !== "") {
      url = `${url}${url.includes("?") ? "&" : "?"}${suffix}`;
    }
  }
  const init: RequestInit = {
    method: options.method ?? "GET",
    credentials: "same-origin",
    headers: {},
  };
  if (attributedAddonId !== null) {
    (init.headers as Record<string, string>)["X-ZIM-Addon"] = attributedAddonId;
  }
  if (options.body !== undefined) {
    init.body = JSON.stringify(options.body);
    (init.headers as Record<string, string>)["Content-Type"] = "application/json";
  }
  const response = await fetch(url, init);
  if (!response.ok) {
    throw await parseError(response);
  }
  if (response.status === 204) {
    return undefined as T;
  }
  return (await response.json()) as T;
}

/** Resolve this addon's registration; safe to call repeatedly. */
async function init(addonId: string): Promise<SelfSummary | null> {
  attributedAddonId = addonId;
  if (resolvedSelf !== null && resolvedSelf.id === addonId) {
    return resolvedSelf;
  }
  if (initPromise !== null) {
    return initPromise;
  }
  initPromise = (async () => {
    const envelope = await api<ListEnvelope<SelfSummary>>("/api/v1/extensions");
    const found = envelope.data.find((addon) => addon.id === addonId) ?? null;
    resolvedSelf = found;
    return found;
  })();
  return initPromise;
}

async function whoami(): Promise<Me | null> {
  try {
    return await api<Me | null>("/api/v1/me");
  } catch (error) {
    if (error instanceof ApiError && error.isAuth) {
      return null;
    }
    throw error;
  }
}

/** One self-healing §11 WebSocket subscription for one instance. */
class ZimStream {
  private readonly instanceId: string;
  private readonly handlers: StreamHandlers;
  private socket: WebSocket | null = null;
  private lastGeneration = 0;
  private attempts = 0;
  private closed = false;
  private reconnectTimer: ReturnType<typeof setTimeout> | undefined;

  constructor(instanceId: string, handlers: StreamHandlers) {
    this.instanceId = instanceId;
    this.handlers = handlers;
    this.connect();
  }

  get generation(): number {
    return this.lastGeneration;
  }

  get connected(): boolean {
    return this.socket !== null && this.socket.readyState === WebSocket.OPEN;
  }

  close(): void {
    this.closed = true;
    if (this.reconnectTimer !== undefined) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = undefined;
    }
    if (this.socket !== null) {
      const socket = this.socket;
      this.socket = null;
      socket.close();
    }
    this.handlers.onStatus?.("down");
  }

  private connect(): void {
    if (this.closed) {
      return;
    }
    this.handlers.onStatus?.(this.attempts === 0 ? "connecting" : "reconnecting");
    const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    const query = new URLSearchParams({ instance_id: this.instanceId });
    if (this.lastGeneration > 0) {
      query.set("since_generation", String(this.lastGeneration));
    }
    const url = `${protocol}//${window.location.host}/api/v1/stream?${query.toString()}`;
    let socket: WebSocket;
    try {
      socket = new WebSocket(url, ["zim-events.v1"]);
    } catch (error) {
      this.handlers.onFatal?.(String(error));
      return;
    }
    this.socket = socket;

    socket.onopen = () => {
      this.attempts = 0;
      this.handlers.onStatus?.("live");
    };

    socket.onmessage = (message: MessageEvent<string>) => {
      if (typeof message.data !== "string") {
        return;
      }
      let event: StreamEvent;
      try {
        event = JSON.parse(message.data) as StreamEvent;
      } catch {
        return;
      }
      if (typeof event.generation === "number" && event.generation > this.lastGeneration) {
        this.lastGeneration = event.generation;
        this.handlers.onGeneration?.(this.lastGeneration);
      }
      if (event.type === "stream.gap") {
        const from = typeof event.data.from === "number" ? event.data.from : 0;
        const to = typeof event.data.to === "number" ? event.data.to : 0;
        this.handlers.onGap?.(from, to);
      }
      this.handlers.onEvent(event);
    };

    socket.onclose = () => {
      if (this.closed || this.socket !== socket) {
        return;
      }
      this.socket = null;
      this.attempts += 1;
      if (this.attempts > MAX_ATTEMPTS) {
        this.handlers.onFatal?.("the stream did not recover after repeated failures");
        return;
      }
      const backoff = Math.min(
        BACKOFF_CAP_MS,
        BACKOFF_FLOOR_MS * Math.pow(2, this.attempts - 1),
      );
      this.reconnectTimer = setTimeout(() => {
        this.reconnectTimer = undefined;
        this.connect();
      }, backoff);
    };
  }
}

// The single global this script contributes (§3.8-style hygiene: one
// name, no DOM access at load time).
(globalThis as { Zim?: unknown }).Zim = {
  init,
  api,
  whoami,
  assertPermission,
  ApiError,
  ZimStream,
};
