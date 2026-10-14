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
    constructor(status, code, message, field, requestId, missingPermissions = []) {
        super(message);
        this.name = "ApiError";
        this.status = status;
        this.code = code;
        this.field = field;
        this.requestId = requestId;
        this.missingPermissions = missingPermissions;
    }
    /** Whether this error is an authentication failure. */
    get isAuth() {
        return this.status === 401;
    }
    /** Whether this error is an authorization failure. */
    get isPermission() {
        return this.status === 403;
    }
}
/** Parse a §6 structured error body. */
async function parseError(response) {
    let code = "network_error";
    let message = `${response.status} ${response.statusText || "(no status text)"}`;
    let field;
    let requestId;
    let missing = [];
    try {
        const body = await response.json();
        const err = body && body.error;
        if (err) {
            code = typeof err.code === "string" ? err.code : code;
            message = typeof err.message === "string" ? err.message : message;
            field = typeof err.field === "string" ? err.field : undefined;
            requestId = typeof err.request_id === "string" ? err.request_id : undefined;
            if (Array.isArray(err.missing_permissions)) {
                missing = err.missing_permissions.filter((p) => typeof p === "string");
            }
        }
    }
    catch {
        // Body was not JSON — keep the HTTP-level fallback text.
    }
    const error = new ApiError(response.status, code, message, field, requestId, missing);
    return error;
}
/** Perform one API call and return the parsed JSON body. */
export async function api(path, options = {}) {
    const init = {
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
        return undefined;
    }
    return (await response.json());
}
// ── Typed endpoint helpers ────────────────────────────────────────
/** Engine health (unauthenticated). */
export async function health() {
    return api("/health");
}
/** The signed-in user, or `null` when the session is absent/expired. */
export async function whoami() {
    try {
        return await api("/users/me");
    }
    catch (error) {
        if (error instanceof ApiError && error.isAuth) {
            return null;
        }
        throw error;
    }
}
/** Sign in; the engine sets the session cookie. */
export async function login(email, password) {
    return api("/auth/login", { body: { email, password } });
}
/** Sign out; the engine clears the session cookie. */
export async function logout() {
    await api("/auth/logout", { method: "POST" });
}
/** List the caller-visible instances (one page, large). */
export async function listInstances(perPage = 100) {
    const envelope = await api(`/instances?per_page=${perPage}`);
    return envelope.data;
}
/** One instance's row. */
export async function getInstance(id) {
    return api(`/instances/${encodeURIComponent(id)}`);
}
/** One instance's observed state. */
export async function getState(id) {
    return api(`/instances/${encodeURIComponent(id)}/state`);
}
/** One instance's console buffer. */
export async function consoleBuffer(id) {
    return api(`/instances/${encodeURIComponent(id)}/console`);
}
/** Send one line of console input. */
export async function consoleInput(id, line) {
    await api(`/instances/${encodeURIComponent(id)}/console/input`, { body: { line } });
}
/** A power operation: `start`, `stop`, `restart`, `kill` or `suspend`. */
export async function power(id, operation) {
    await api(`/instances/${encodeURIComponent(id)}/${operation}`, { method: "POST" });
}
/** The active frontend package's info (M11). */
export async function frontendActive() {
    return api("/frontend/active");
}
/** Map an engine status string onto the §3 visual model. */
export function visualStatus(status) {
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
            return { color: "blue", label: "suspended" };
        default:
            return { color: "gray", label: status || "unknown" };
    }
}
/** Format an ISO timestamp as a local human string. */
export function formatTime(iso) {
    const date = new Date(iso);
    if (Number.isNaN(date.getTime())) {
        return iso;
    }
    return date.toLocaleString();
}
