"use strict";
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
    code;
    status;
    field;
    missingPermissions;
    requestId;
    constructor(status, code, message, field, requestId, missingPermissions = []) {
        super(message);
        this.name = "ZimApiError";
        this.status = status;
        this.code = code;
        this.field = field;
        this.requestId = requestId;
        this.missingPermissions = missingPermissions;
    }
    get isAuth() {
        return this.status === 401;
    }
    get isPermission() {
        return this.status === 403;
    }
}
const BACKOFF_FLOOR_MS = 1_000;
const BACKOFF_CAP_MS = 15_000;
const MAX_ATTEMPTS = 6;
/** The resolved registration of the addon this script serves. */
let resolvedSelf = null;
let initPromise = null;
/** This page's addon id, captured at Zim.init (AD-29's attribution:
 * rides every API call as X-ZIM-Addon so the engine's audit log can
 * attribute the action to the addon whose page made it — advisory
 * by design; the user's own credentials still authorize the call). */
let attributedAddonId = null;
async function parseError(response) {
    let code = "network_error";
    let message = `${response.status} ${response.statusText || "(no status text)"}`;
    let field;
    let requestId;
    let missing = [];
    try {
        const body = (await response.json());
        const err = body && body.error;
        if (err) {
            if (typeof err.code === "string")
                code = err.code;
            if (typeof err.message === "string")
                message = err.message;
            if (typeof err.field === "string")
                field = err.field;
            if (typeof err.request_id === "string")
                requestId = err.request_id;
            if (Array.isArray(err.missing_permissions)) {
                missing = err.missing_permissions.filter((p) => typeof p === "string");
            }
        }
    }
    catch {
        // Body was not JSON — keep the HTTP-level fallback text.
    }
    return new ApiError(response.status, code, message, field, requestId, missing);
}
function assertPermission(name) {
    if (resolvedSelf === null) {
        throw new ApiError(0, "not_initialized", "Zim.init(addonId) must resolve before gated calls");
    }
    if (!resolvedSelf.permissions.granted.includes(name)) {
        throw new ApiError(403, "permission_denied", `this addon was not granted ${JSON.stringify(name)}`, undefined, undefined, [name]);
    }
}
async function api(path, options = {}) {
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
    const init = {
        method: options.method ?? "GET",
        credentials: "same-origin",
        headers: {},
    };
    if (attributedAddonId !== null) {
        init.headers["X-ZIM-Addon"] = attributedAddonId;
    }
    if (options.body !== undefined) {
        init.body = JSON.stringify(options.body);
        init.headers["Content-Type"] = "application/json";
    }
    const response = await fetch(url, init);
    if (!response.ok) {
        throw await parseError(response);
    }
    if (response.status === 204) {
        return undefined;
    }
    return (await response.json());
}
/** Resolve this addon's registration; safe to call repeatedly. */
async function init(addonId) {
    attributedAddonId = addonId;
    if (resolvedSelf !== null && resolvedSelf.id === addonId) {
        return resolvedSelf;
    }
    if (initPromise !== null) {
        return initPromise;
    }
    initPromise = (async () => {
        const envelope = await api("/api/v1/extensions");
        const found = envelope.data.find((addon) => addon.id === addonId) ?? null;
        resolvedSelf = found;
        return found;
    })();
    return initPromise;
}
async function whoami() {
    try {
        return await api("/api/v1/me");
    }
    catch (error) {
        if (error instanceof ApiError && error.isAuth) {
            return null;
        }
        throw error;
    }
}
/** One self-healing §11 WebSocket subscription for one instance. */
class ZimStream {
    instanceId;
    handlers;
    socket = null;
    lastGeneration = 0;
    attempts = 0;
    closed = false;
    reconnectTimer;
    constructor(instanceId, handlers) {
        this.instanceId = instanceId;
        this.handlers = handlers;
        this.connect();
    }
    get generation() {
        return this.lastGeneration;
    }
    get connected() {
        return this.socket !== null && this.socket.readyState === WebSocket.OPEN;
    }
    close() {
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
    connect() {
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
        let socket;
        try {
            socket = new WebSocket(url, ["zim-events.v1"]);
        }
        catch (error) {
            this.handlers.onFatal?.(String(error));
            return;
        }
        this.socket = socket;
        socket.onopen = () => {
            this.attempts = 0;
            this.handlers.onStatus?.("live");
        };
        socket.onmessage = (message) => {
            if (typeof message.data !== "string") {
                return;
            }
            let event;
            try {
                event = JSON.parse(message.data);
            }
            catch {
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
            const backoff = Math.min(BACKOFF_CAP_MS, BACKOFF_FLOOR_MS * Math.pow(2, this.attempts - 1));
            this.reconnectTimer = setTimeout(() => {
                this.reconnectTimer = undefined;
                this.connect();
            }, backoff);
        };
    }
}
// The single global this script contributes (§3.8-style hygiene: one
// name, no DOM access at load time).
globalThis.Zim = {
    init,
    api,
    whoami,
    assertPermission,
    ApiError,
    ZimStream,
};
