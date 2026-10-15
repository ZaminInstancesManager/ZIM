/**
 * ZIM default frontend — engine API client.
 *
 * Thin, dependency-free fetch wrapper over the loopback engine API
 * (`/api/v1`, same origin). Session credentials ride the engine-set
 * `zim_session` cookie (HttpOnly, Path=/api/v1), so every call uses
 * `credentials: "same-origin"` and nothing is stored in JS.
 *
 * Structured errors follow ZIM_API_SPEC.md §6:
 * `{ "error": { "code", "message", "request_id", ... } }`.
 */

/** One structured engine error (`docs/api/ZIM_API_SPEC.md` §6). */
export class ApiError extends Error {
  /** The stable `code` string (`bad_request`, `permission_denied`, …). */
  readonly code: string;
  /** The HTTP status the engine answered with. */
  readonly status: number;
  /** The offending field, when the engine identified one. */
  readonly field?: string;
  /** Missing permission grants, when the engine listed them. */
  readonly missingPermissions: string[];
  /** The engine's correlation id for this failed request. */
  readonly requestId?: string;

  constructor(
    status: number,
    code: string,
    message: string,
    field?: string,
    requestId?: string,
    missingPermissions: string[] = [],
  ) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.field = field;
    this.requestId = requestId;
    this.missingPermissions = missingPermissions;
  }

  /** Whether this error is an authentication failure. */
  get isAuth(): boolean {
    return this.status === 401;
  }

  /** Whether this error is an authorization failure. */
  get isPermission(): boolean {
    return this.status === 403;
  }
}

/** Parse a §6 structured error body. */
async function parseError(response: Response): Promise<ApiError> {
  let code = "network_error";
  let message = `${response.status} ${response.statusText || "(no status text)"}`;
  let field: string | undefined;
  let requestId: string | undefined;
  let missing: string[] = [];
  try {
    const body = await response.json();
    const err = body && body.error;
    if (err) {
      code = typeof err.code === "string" ? err.code : code;
      message = typeof err.message === "string" ? err.message : message;
      field = typeof err.field === "string" ? err.field : undefined;
      requestId = typeof err.request_id === "string" ? err.request_id : undefined;
      if (Array.isArray(err.missing_permissions)) {
        missing = err.missing_permissions.filter((p: unknown): p is string => typeof p === "string");
      }
    }
  } catch {
    // Body was not JSON — keep the HTTP-level fallback text.
  }
  const error = new ApiError(response.status, code, message, field, requestId, missing);
  return error;
}

/** Options for {@link api}. */
export interface ApiOptions {
  /** Request body — serialized as JSON. */
  body?: unknown;
  /** HTTP method (default `GET`, or `POST` when a body is present). */
  method?: string;
}

/** Perform one API call and return the parsed JSON body. */
export async function api<T = unknown>(path: string, options: ApiOptions = {}): Promise<T> {
  const init: RequestInit = {
    credentials: "same-origin",
    method: options.method ?? (options.body !== undefined ? "POST" : "GET"),
    headers: { Accept: "application/json" },
  };
  if (options.body !== undefined) {
    init.headers = { Accept: "application/json", "Content-Type": "application/json" };
    init.body = JSON.stringify(options.body);
  }
  const response = await fetch(`/api/v1${path}`, init);
  if (!response.ok) {
    throw await parseError(response);
  }
  if (response.status === 204) {
    return undefined as T;
  }
  return (await response.json()) as T;
}

// ── Shared wire types (the engine's serialization shapes) ─────────

/** An instance row (`GET /instances` envelope data element). */
export interface Instance {
  id: string;
  uuid: string;
  uuid_short: string;
  name: string;
  description: string;
  /** Snake-case lifecycle status (`offline`, `running`, …). */
  status: string;
  node_id: string;
  egg_id: string;
  owner_id: string;
  created_at: string;
  updated_at: string;
  [key: string]: unknown;
}

/** The pagination envelope (`object: "list"` pages). */
export interface ListEnvelope<T> {
  object: "list";
  data: T[];
  meta: {
    total: number;
    page: number;
    per_page: number;
    [key: string]: unknown;
  };
}

/** The observed-state snapshot (`GET /instances/{id}/state`). */
export interface StateSnapshot {
  id: string;
  status: string;
  observed_by: string;
  updated_at: string;
  generation?: number;
  in_crash_loop?: boolean;
  observed?: { state: string; [key: string]: unknown };
}

/** The console buffer (`GET /instances/{id}/console`). */
export interface ConsoleBuffer {
  instance_id: string;
  events: StreamEvent[];
  last_generation: number;
}

/** One §11.1 stream event (as serialized on the wire). */
export interface StreamEvent {
  type: string;
  generation: number;
  ts: string;
  instance_id: string | null;
  data: {
    from?: string;
    to?: string;
    line?: string;
    dropped_events?: number;
    [key: string]: unknown;
  };
}

/** The signed-in user (`GET /users/me`). */
export interface Me {
  id: string;
  email: string;
  username: string;
  is_admin: boolean;
  root_admin: boolean;
  [key: string]: unknown;
}

/** The active-frontend info (`GET /frontend/active`). */
export interface FrontendActive {
  object: string;
  id: string;
  version: string;
  name: string;
  is_default: boolean;
  origin: string;
  page_surfaces: string[];
  smoke: string;
  recovery_notice?: { reason: string; at: string } | null;
  [key: string]: unknown;
}

// ── Typed endpoint helpers ────────────────────────────────────────

/** Engine health (unauthenticated). */
export async function health(): Promise<{ status: string; service: string; api_version: string }> {
  return api("/health");
}

/** The signed-in user, or `null` when the session is absent/expired. */
export async function whoami(): Promise<Me | null> {
  try {
    return await api("/users/me");
  } catch (error) {
    if (error instanceof ApiError && error.isAuth) {
      return null;
    }
    throw error;
  }
}

/** Sign in; the engine sets the session cookie. */
export async function login(email: string, password: string): Promise<Me> {
  return api("/auth/login", { body: { email, password } });
}

/** Sign out; the engine clears the session cookie. */
export async function logout(): Promise<void> {
  await api("/auth/logout", { method: "POST" });
}

/** List the caller-visible instances (one page, large). */
export async function listInstances(perPage = 100): Promise<Instance[]> {
  const envelope = await api<ListEnvelope<Instance>>(`/instances?per_page=${perPage}`);
  return envelope.data;
}

/** One instance's row. */
export async function getInstance(id: string): Promise<Instance> {
  return api(`/instances/${encodeURIComponent(id)}`);
}

/** One instance's observed state. */
export async function getState(id: string): Promise<StateSnapshot> {
  return api(`/instances/${encodeURIComponent(id)}/state`);
}

/** One instance's console buffer. */
export async function consoleBuffer(id: string): Promise<ConsoleBuffer> {
  return api(`/instances/${encodeURIComponent(id)}/console`);
}

/** Send one line of console input. */
export async function consoleInput(id: string, line: string): Promise<void> {
  await api(`/instances/${encodeURIComponent(id)}/console/input`, { body: { line } });
}

/** A power operation: `start`, `stop`, `restart`, `kill` or `suspend`. */
export async function power(id: string, operation: string): Promise<void> {
  await api(`/instances/${encodeURIComponent(id)}/${operation}`, { method: "POST" });
}

/** The active frontend package's info (M11). */
export async function frontendActive(): Promise<FrontendActive> {
  return api("/frontend/active");
}

/** The recent resource-usage view (`GET /instances/{id}/metrics`):
 * the last §11.1 `instance.resource_usage` events. */
export interface MetricsSnapshot {
  instance_id: string;
  samples: StreamEvent[];
  last_generation: number;
}

/** The recent resource-usage samples for one instance (M7). */
export async function metricsSnapshot(id: string): Promise<MetricsSnapshot> {
  return api(`/instances/${encodeURIComponent(id)}/metrics`);
}

// ── Status visual model (BROWSER_INTEGRATION.md §4) ───────────────

/** The visual classification of a lifecycle status. */
export interface VisualStatus {
  /** One of the four primary colors, or `gray`. */
  color: "green" | "yellow" | "red" | "blue" | "gray";
  /** A short human label for the state. */
  label: string;
}

/** Map an engine status string onto the §4 visual model (the
 * engine's canonical `visual_color`, mirrored). */
export function visualStatus(status: string): VisualStatus {
  switch (status) {
    case "running":
      return { color: "green", label: "running" };
    case "starting":
      return { color: "yellow", label: "starting" };
    case "stopping":
      return { color: "yellow", label: "stopping" };
    case "installing":
      return { color: "yellow", label: "installing" };
    case "restoring_backup":
      return { color: "yellow", label: "restoring backup" };
    case "transferring":
      return { color: "yellow", label: "transferring" };
    case "crash_loop":
      return { color: "red", label: "crash loop" };
    case "offline":
      return { color: "blue", label: "offline" };
    case "suspended":
      return { color: "gray", label: "suspended" };
    default:
      return { color: "gray", label: status || "unknown" };
  }
}

/** Format an ISO timestamp as a local human string. */
export function formatTime(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) {
    return iso;
  }
  return date.toLocaleString();
}
