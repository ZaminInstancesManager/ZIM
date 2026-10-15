/**
 * The harness's engine API client: JSON over the engine's loopback
 * TLS origin with the session cookie the real UI uses. This is the
 * *setup and assertion* channel; user-visible behavior is driven
 * through the browser (lib/browser.ts), not through here.
 */

import type { EngineHandle } from "./engine.js";

// The engine serves a self-signed loopback certificate by design
// (docs/architecture/RUST_ENGINE.md §loopback TLS); the shell's
// answer is installing the cert into the OS trust store, and ours
// is to not validate TLS *for this process* — every URL the suite
// fetches is `https://127.0.0.1:{engine port}`.
process.env.NODE_TLS_REJECT_UNAUTHORIZED = "0";

/** A structured API failure, carrying the engine's own error body. */
export class ApiError extends Error {
  readonly status: number;
  readonly body: string;

  constructor(status: number, body: string) {
    super(`API ${status}: ${body.slice(0, 400)}`);
    this.status = status;
    this.body = body;
  }
}

export interface ApiClient {
  /** Login (JSON, spec §3) and hold the session cookie. */
  login(email: string, password: string): Promise<void>;
  /** A GET returning the parsed body (asserts 2xx). */
  get(path: string): Promise<any>;
  /** A GET returning the raw status + text (negative-path asserts). */
  tryGet(path: string): Promise<{ status: number; body: string }>;
  /** A JSON POST (asserts 2xx). */
  post(path: string, body?: unknown): Promise<any>;
  /** A JSON POST with an explicit content type (raw upload bodies). */
  postRaw(path: string, body: Buffer | string, contentType: string): Promise<any>;
  /** A JSON PUT (asserts 2xx). */
  put(path: string, body?: unknown): Promise<any>;
  /** A JSON PATCH (asserts 2xx). */
  patch(path: string, body?: unknown): Promise<any>;
  /** A DELETE (asserts 2xx). */
  del(path: string): Promise<any>;
}

/** Build a logged-in client in one step (the common harness shape). */
export async function adminClient(engine: EngineHandle): Promise<ApiClient> {
  const client = makeClient(engine);
  await client.login(engine.admin.email, engine.admin.password);
  return client;
}

// Allocation pools, keyed per client: a fresh engine's local node
// ships ZERO free allocations, so the first instance creation
// would 409 ("node \"local\" has no free allocation"). The pool is
// created lazily by createInstance, once per (client, node), with
// ports the harness picked as free moments earlier — the containers
// bind them only when instances start, and every test file owns its
// own engine, so cross-engine collisions are a TOCTOU rounding error.
const ensuredNodesByClient = new WeakMap<ApiClient, Set<string>>();

function ensuredNodesFor(client: ApiClient): Set<string> {
  let nodes = ensuredNodesByClient.get(client);
  if (nodes === undefined) {
    nodes = new Set<string>();
    ensuredNodesByClient.set(client, nodes);
  }
  return nodes;
}

/** Build a client without authenticating. */
export function makeClient(engine: EngineHandle): ApiClient {
  let cookie: string | null = null;

  async function request(
    method: string,
    path: string,
    body?: Buffer | string,
    contentType?: string,
  ): Promise<Response> {
    const headers: Record<string, string> = {};
    if (cookie !== null) {
      headers["Cookie"] = cookie;
    }
    if (body !== undefined) {
      headers["Content-Type"] = contentType ?? "application/json";
    }
    return await fetch(`${engine.origin}${path}`, {
      method,
      headers,
      body,
      signal: AbortSignal.timeout(30_000),
    });
  }

  async function readError(response: Response): Promise<never> {
    const body = await response.text();
    throw new ApiError(response.status, body);
  }

  return {
    async login(email: string, password: string): Promise<void> {
      const response = await request("POST", "/api/v1/auth/login", JSON.stringify({ email, password }));
      if (response.status !== 200 && response.status !== 201) {
        const body = await response.text();
        throw new ApiError(response.status, body);
      }
      // Node's fetch exposes every Set-Cookie via getSetCookie().
      const set = response.headers.getSetCookie();
      const session = set.find((value) => value.startsWith("zim_session="));
      if (session === undefined) {
        throw new Error("login response carried no zim_session cookie");
      }
      // The cookie's attributes ride along; sending them back is
      // harmless (servers read the name=value pair).
      cookie = session.split(";")[0] ?? null;
    },

    async get(path: string): Promise<any> {
      const response = await request("GET", path);
      if (!response.ok) {
        await readError(response);
      }
      const text = await response.text();
      return text === "" ? null : JSON.parse(text);
    },

    async tryGet(path: string): Promise<{ status: number; body: string }> {
      const response = await request("GET", path);
      return { status: response.status, body: await response.text() };
    },

    async post(path: string, body?: unknown): Promise<any> {
      const payload = body === undefined ? undefined : JSON.stringify(body);
      const response = await request("POST", path, payload);
      if (!response.ok) {
        await readError(response);
      }
      const text = await response.text();
      return text === "" ? null : JSON.parse(text);
    },

    async postRaw(path: string, body: Buffer | string, contentType: string): Promise<any> {
      const response = await request("POST", path, body, contentType);
      if (!response.ok) {
        await readError(response);
      }
      const text = await response.text();
      return text === "" ? null : JSON.parse(text);
    },

    async put(path: string, body?: unknown): Promise<any> {
      const payload = body === undefined ? undefined : JSON.stringify(body);
      const response = await request("PUT", path, payload);
      if (!response.ok) {
        await readError(response);
      }
      const text = await response.text();
      return text === "" ? null : JSON.parse(text);
    },

    async patch(path: string, body?: unknown): Promise<any> {
      const payload = body === undefined ? undefined : JSON.stringify(body);
      const response = await request("PATCH", path, payload);
      if (!response.ok) {
        await readError(response);
      }
      const text = await response.text();
      return text === "" ? null : JSON.parse(text);
    },

    async del(path: string): Promise<any> {
      const response = await request("DELETE", path);
      if (!response.ok) {
        await readError(response);
      }
      const text = await response.text();
      return text === "" ? null : JSON.parse(text);
    },
  };
}

// ── Domain helpers (thin, typed where the tests care) ────────────

/** The single seeded local node's id (every fresh engine has one). */
export async function firstNodeId(client: ApiClient): Promise<string> {
  const page = await client.get("/api/v1/nodes?per_page=5");
  const nodes: unknown[] = page.data;
  if (!Array.isArray(nodes) || nodes.length === 0) {
    throw new Error("no nodes listed on a fresh engine");
  }
  return String((nodes[0] as Record<string, unknown>).id);
}

export interface CreatedInstance {
  id: string;
  name: string;
}

/**
 * Ensure the node owns a small free-allocation pool (ports the
 * harness picked as free moments earlier). Callers normally go
 * through `createInstance`, which memoizes per (client, node).
 */
export async function ensureAllocationPool(
  client: ApiClient,
  nodeId: string,
  count = 6,
): Promise<void> {
  const { freePort } = await import("./util.js");
  for (let index = 0; index < count; index += 1) {
    const port = await freePort();
    await client.post(`/api/v1/nodes/${nodeId}/allocations`, { port });
  }
}

/** Create an instance (admin) with the busybox tick command. */
export async function createInstance(
  client: ApiClient,
  name: string,
  options: {
    nodeId: string;
    image?: string;
    startup?: string;
    owner_id?: string;
  },
): Promise<CreatedInstance> {
  // A fresh engine's node has no free allocations — create the
  // pool once per (client, node) before the first instance.
  const ensured = ensuredNodesFor(client);
  if (!ensured.has(options.nodeId)) {
    await ensureAllocationPool(client, options.nodeId);
    ensured.add(options.nodeId);
  }
  const instance = await client.post("/api/v1/instances", {
    name,
    node_id: options.nodeId,
    image: options.image ?? "busybox:latest",
    startup:
      options.startup ??
      "sh -c 'while true; do echo zim-e2e-tick; sleep 1; done'",
  });
  return { id: String(instance.id), name: String(instance.name) };
}

/** The observed state envelope for one instance. */
export async function instanceState(
  client: ApiClient,
  id: string,
): Promise<{ status: string }> {
  return (await client.get(`/api/v1/instances/${id}/state`)) as { status: string };
}

/** The audit log's events (admin), newest-first page 1. */
export async function auditEvents(client: ApiClient, limit = 50): Promise<Array<Record<string, unknown>>> {
  const page = await client.get(`/api/v1/audit?limit=${limit}`);
  const events = page.data ?? page.events;
  if (!Array.isArray(events)) {
    throw new Error(`unexpected audit envelope: ${JSON.stringify(page).slice(0, 300)}`);
  }
  return events as Array<Record<string, unknown>>;
}

/** Audit events whose `action` matches one of `actions`. */
export async function auditWhere(
  client: ApiClient,
  actions: string[],
  limit = 100,
): Promise<Array<Record<string, unknown>>> {
  const events = await auditEvents(client, limit);
  return events.filter((event) => actions.includes(String(event.action)));
}

/** The buffered console output lines for one instance. */
export async function consoleLines(client: ApiClient, id: string): Promise<string[]> {
  const buffer = await client.get(`/api/v1/instances/${id}/console`);
  const events: Array<Record<string, unknown>> = buffer.events ?? [];
  return events
    .filter((event) => String(event.type) === "instance.console_output")
    .map((event) => {
      const data = event.data as Record<string, unknown> | undefined;
      return String(data?.line ?? "");
    });
}
