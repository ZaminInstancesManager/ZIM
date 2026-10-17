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
/** Engine health (unauthenticated). M27: `version` is the RUNNING
 * engine's build version (absent only on pre-M27 engines). */
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
// ── Recent servers, nodes, instance creation (M23 NTP) ───────────
/** The caller's recent servers (most-recent first). */
export async function listRecents(limit = 8) {
    const clamped = Math.min(Math.max(Math.floor(limit), 1), 32);
    const body = await api(`/me/recents?limit=${clamped}`);
    return body.data;
}
/** Record that the caller opened a server view (fire-and-forget by
 * design: recents are a nicety — failures are swallowed by the
 * caller, never surfaced as chrome errors). */
export async function recordRecent(instanceId) {
    await api("/me/recents", { body: { instance_id: instanceId } });
}
/** List nodes (admin-visible; the create dialog degrades honestly
 * when this is a 403). */
export async function listNodes() {
    const envelope = await api("/nodes");
    return envelope.data;
}
/** Create a server (admin-only engine-side; 201 returns the row). */
export async function createInstance(request) {
    return api("/instances", { body: request });
}
/** The recent resource-usage samples for one instance (M7). */
export async function metricsSnapshot(id) {
    return api(`/instances/${encodeURIComponent(id)}/metrics`);
}
/** Map an engine status string onto the §4 visual model (the
 * engine's canonical `visual_color`, mirrored). */
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
            return { color: "gray", label: "suspended" };
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
/** `GET /extensions` — every installed addon plus boot notices. */
export async function listExtensions() {
    return api("/extensions");
}
/** `POST /extensions` — install (or update) an addon from `.zimpkg`
 * ZIP bytes (admin; the upload IS the approval). */
export async function installExtension(packageBytes) {
    const response = await fetch("/api/v1/extensions", {
        credentials: "same-origin",
        method: "POST",
        headers: { "Content-Type": "application/zip", Accept: "application/json" },
        body: packageBytes,
    });
    if (!response.ok) {
        throw await parseError(response);
    }
    return (await response.json());
}
/** `POST /extensions/{id}/enable` or `…/disable` (admin). */
export async function setExtensionEnabled(id, enabled) {
    return api(`/extensions/${encodeURIComponent(id)}/${enabled ? "enable" : "disable"}`, {
        method: "POST",
    });
}
/** `DELETE /extensions/{id}` — uninstall + revoke (admin). */
export async function uninstallExtension(id) {
    return api(`/extensions/${encodeURIComponent(id)}`, { method: "DELETE" });
}
/** `POST /extensions/{id}/grant` — grant additional permissions
 * (admin; §4.2's elevation re-approval). */
export async function grantExtension(id, permissions) {
    return api(`/extensions/${encodeURIComponent(id)}/grant`, {
        body: { permissions },
    });
}
/** `GET /themes` — the uploaded themes (the presets live in the
 * frontend's own code). */
export async function listThemes() {
    return api("/themes");
}
/** `POST /themes` — install a packaged theme (`.zip` with
 * `zim.theme.json` at the root; admin). */
export async function uploadTheme(zipBytes) {
    const response = await fetch("/api/v1/themes", {
        credentials: "same-origin",
        method: "POST",
        headers: { "Content-Type": "application/zip", Accept: "application/json" },
        body: zipBytes,
    });
    if (!response.ok) {
        throw await parseError(response);
    }
    return (await response.json());
}
/** `DELETE /themes/{id}` — remove an uploaded theme (admin). */
export async function deleteTheme(id) {
    return api(`/themes/${encodeURIComponent(id)}`, { method: "DELETE" });
}
/** `GET /instances/{id}/files?path={dir}` — list one directory. */
export async function listFiles(instanceId, path = ".") {
    const suffix = path === "." || path === "" ? "" : `?path=${encodeURIComponent(path)}`;
    return api(`/instances/${encodeURIComponent(instanceId)}/files${suffix}`);
}
/** `GET /instances/{id}/files/content?path={file}` — one file's raw
 * bytes (`application/octet-stream` download). */
export async function downloadFile(instanceId, path) {
    const response = await fetch(`/api/v1/instances/${encodeURIComponent(instanceId)}/files/content?path=${encodeURIComponent(path)}`, { credentials: "same-origin", headers: { Accept: "application/octet-stream" } });
    if (!response.ok) {
        throw await parseError(response);
    }
    return response.arrayBuffer();
}
/** `PUT /instances/{id}/files/content?path={file}` — write one file's
 * bytes (the atomic upload channel; parents created as needed). */
export async function uploadFile(instanceId, path, bytes) {
    const response = await fetch(`/api/v1/instances/${encodeURIComponent(instanceId)}/files/content?path=${encodeURIComponent(path)}`, {
        credentials: "same-origin",
        method: "PUT",
        headers: { "Content-Type": "application/octet-stream", Accept: "application/json" },
        body: bytes,
    });
    if (!response.ok) {
        throw await parseError(response);
    }
    return (await response.json());
}
/** `POST /instances/{id}/files/create` — create one file or
 * directory (`is_dir`; a file starts empty). */
export async function createFileEntry(instanceId, path, isDir) {
    return api(`/instances/${encodeURIComponent(instanceId)}/files/create`, { body: { path, is_dir: isDir } });
}
/** `DELETE /instances/{id}/files?path={path}` — delete one file or
 * directory (directories recursively). */
export async function deleteFileEntry(instanceId, path) {
    const suffix = path === "." || path === "" ? "" : `?path=${encodeURIComponent(path)}`;
    return api(`/instances/${encodeURIComponent(instanceId)}/files${suffix}`, {
        method: "DELETE",
    });
}
/** `POST /instances/{id}/files/rename` — rename or move one path
 * (destination parents created as needed). */
export async function renameFileEntry(instanceId, request) {
    return api(`/instances/${encodeURIComponent(instanceId)}/files/rename`, { body: request });
}
/** `POST /instances/{id}/files/archive` — compress an allow-list of
 * paths into one archive inside the instance root. */
export async function archiveFiles(instanceId, request) {
    return api(`/instances/${encodeURIComponent(instanceId)}/files/archive`, { body: request });
}
/** `POST /instances/{id}/files/extract` — extract one archive under
 * a destination inside the instance root. */
export async function extractArchive(instanceId, request) {
    return api(`/instances/${encodeURIComponent(instanceId)}/files/extract`, { body: request });
}
/** The join of one directory path and one entry name (root-relative,
 * forward slashes — the §10.3 path grammar). */
export function joinFilePath(directory, name) {
    const trimmed = directory === "." || directory === "" ? "" : directory.replace(/\/+$/, "");
    return trimmed === "" ? name : `${trimmed}/${name}`;
}
/** One path's parent, as the listing API expects it (`"."` for the
 * root's children). */
export function parentFilePath(path) {
    const trimmed = path.replace(/\/+$/, "");
    const lastSlash = trimmed.lastIndexOf("/");
    if (lastSlash === -1) {
        return ".";
    }
    return trimmed.slice(0, lastSlash);
}
/** Whether a job state is terminal — the engine's `JobState`
 * vocabulary (`succeeded`, `failed`, `cancelled`); anything else
 * (`pending`, `in_progress`, `cleanup_pending`) is still moving. */
export function jobIsTerminal(state) {
    return state === "succeeded" || state === "failed" || state === "cancelled";
}
/** Format a byte count for humans (binary units, KB-free) — the
 * file manager's convention, shared by the management surfaces. */
export function formatBytes(size) {
    if (size < 1024) {
        return `${size} B`;
    }
    const units = ["KiB", "MiB", "GiB", "TiB"];
    let value = size;
    let unit = -1;
    do {
        value /= 1024;
        unit += 1;
    } while (value >= 1024 && unit < units.length - 1);
    return `${value.toFixed(value >= 100 ? 0 : 1)} ${units[unit]}`;
}
/** `GET /instances/{id}/backups` — the backup rows (sealed and
 * in-progress together). */
export async function listBackups(instanceId) {
    const body = await api(`/instances/${encodeURIComponent(instanceId)}/backups`);
    return body.data;
}
/** `POST /instances/{id}/backups` — create a backup: answers `202`'s
 * body (both ids + the job's initial state); the row seals when the
 * durable job completes. */
export async function createBackup(instanceId) {
    return api(`/instances/${encodeURIComponent(instanceId)}/backups`, { method: "POST" });
}
/** `POST .../backups/{bid}/restore` — restore a backup: extraction
 * lands in a staging directory and swaps into place, so a failed
 * restore never corrupts the live data. */
export async function restoreBackup(instanceId, backupId) {
    return api(`/instances/${encodeURIComponent(instanceId)}/backups/${encodeURIComponent(backupId)}/restore`, { method: "POST" });
}
/** `DELETE .../backups/{bid}` — delete a backup (its archive and row
 * together). A locked backup refuses with `409 conflict`. */
export async function deleteBackup(instanceId, backupId) {
    return api(`/instances/${encodeURIComponent(instanceId)}/backups/${encodeURIComponent(backupId)}`, { method: "DELETE" });
}
/** `GET .../backups/{bid}/download` — the archive's raw bytes
 * (`application/gzip`, `Content-Disposition: attachment`). */
export async function downloadBackup(instanceId, backupId) {
    const response = await fetch(`/api/v1/instances/${encodeURIComponent(instanceId)}/backups/${encodeURIComponent(backupId)}/download`, { credentials: "same-origin", headers: { Accept: "application/gzip" } });
    if (!response.ok) {
        throw await parseError(response);
    }
    return response.arrayBuffer();
}
/** `POST .../backups/{bid}/lock` / `DELETE` — toggle the deletion
 * lock; both answer the updated row. */
export async function setBackupLock(instanceId, backupId, locked) {
    return api(`/instances/${encodeURIComponent(instanceId)}/backups/${encodeURIComponent(backupId)}/lock`, { method: locked ? "POST" : "DELETE" });
}
/** `GET /jobs/{jid}` — one durable job's row (the create/restore
 * progress oracle). */
export async function getJob(jobId) {
    return api(`/jobs/${encodeURIComponent(jobId)}`);
}
