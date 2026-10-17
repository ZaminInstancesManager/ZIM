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

/** One recent-servers row (`GET /me/recents`, M23): engine-backed
 * app state per §11 — not a browser-history database. */
export interface RecentEntry {
  instance_id: string;
  last_opened_at: string;
  open_count: number;
}

/** One node row (`GET /nodes`) — the create-server dialog's node list. */
export interface NodeRow {
  id: string;
  name: string;
  status?: string;
  [key: string]: unknown;
}

// ── Typed endpoint helpers ────────────────────────────────────────

/** Engine health (unauthenticated). M27: `version` is the RUNNING
 * engine's build version (absent only on pre-M27 engines). */
export async function health(): Promise<{
  status: string;
  service: string;
  version?: string;
  api_version: string;
}> {
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

// ── Recent servers, nodes, instance creation (M23 NTP) ───────────

/** The caller's recent servers (most-recent first). */
export async function listRecents(limit = 8): Promise<RecentEntry[]> {
  const clamped = Math.min(Math.max(Math.floor(limit), 1), 32);
  const body = await api<{ data: RecentEntry[] }>(`/me/recents?limit=${clamped}`);
  return body.data;
}

/** Record that the caller opened a server view (fire-and-forget by
 * design: recents are a nicety — failures are swallowed by the
 * caller, never surfaced as chrome errors). */
export async function recordRecent(instanceId: string): Promise<void> {
  await api("/me/recents", { body: { instance_id: instanceId } });
}

/** List nodes (admin-visible; the create dialog degrades honestly
 * when this is a 403). */
export async function listNodes(): Promise<NodeRow[]> {
  const envelope = await api<ListEnvelope<NodeRow>>("/nodes");
  return envelope.data;
}

/** The engine's `POST /instances` request shape (create + import). */
export interface CreateInstanceRequest {
  name: string;
  node_id: string;
  owner_id?: string;
  egg_id?: string;
  allocation_id?: number;
  limits?: Record<string, unknown>;
  startup?: string;
  image?: string;
  description?: string;
  external_id?: string;
  allocation_limit?: number;
  database_limit?: number;
  backup_limit?: number;
  crash_detection_enabled?: boolean;
  kind?: string;
}

/** Create a server (admin-only engine-side; 201 returns the row). */
export async function createInstance(request: CreateInstanceRequest): Promise<Instance> {
  return api("/instances", { body: request });
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

// ── Extensions (§10.12, M24) ──────────────────────────────────────

/** One sidebar contribution from an addon's manifest (§4.5). */
export interface AddonSidebarItem {
  label: string;
  icon: string;
  page: string;
}

/** One context-menu contribution from an addon's manifest (§4.6). */
export interface AddonContextMenuItem {
  label: string;
  page: string;
}

/** One command registration from an addon's manifest (§4.7). */
export interface AddonCommand {
  id: string;
  label: string;
  accelerator?: string;
}

/** One installed addon (`GET /extensions` data element). */
export interface AddonSummary {
  id: string;
  version: string;
  name: string;
  description: string;
  author: string;
  license: string;
  enabled: boolean;
  bundled: boolean;
  instance_scope: "required" | "optional" | "none";
  permissions: { requested: string[]; granted: string[] };
  signature: "trusted" | "untrusted" | "unsigned" | "invalid";
  installed_at: string;
  page_surfaces: string[];
  sidebar_items: AddonSidebarItem[];
  context_menu_items: AddonContextMenuItem[];
  commands: AddonCommand[];
  event_subscriptions: string[];
  dependencies: string[];
  notice?: string;
}

/** `GET /extensions` — every installed addon plus boot notices. */
export async function listExtensions(): Promise<{ data: AddonSummary[]; notices: string[] }> {
  return api<{ data: AddonSummary[]; notices: string[] }>("/extensions");
}

/** `POST /extensions` — install (or update) an addon from `.zimpkg`
 * ZIP bytes (admin; the upload IS the approval). */
export async function installExtension(packageBytes: ArrayBuffer): Promise<AddonSummary> {
  const response = await fetch("/api/v1/extensions", {
    credentials: "same-origin",
    method: "POST",
    headers: { "Content-Type": "application/zip", Accept: "application/json" },
    body: packageBytes,
  });
  if (!response.ok) {
    throw await parseError(response);
  }
  return (await response.json()) as AddonSummary;
}

/** `POST /extensions/{id}/enable` or `…/disable` (admin). */
export async function setExtensionEnabled(
  id: string,
  enabled: boolean,
): Promise<AddonSummary> {
  return api<AddonSummary>(`/extensions/${encodeURIComponent(id)}/${enabled ? "enable" : "disable"}`, {
    method: "POST",
  });
}

/** `DELETE /extensions/{id}` — uninstall + revoke (admin). */
export async function uninstallExtension(id: string): Promise<void> {
  return api<void>(`/extensions/${encodeURIComponent(id)}`, { method: "DELETE" });
}

/** `POST /extensions/{id}/grant` — grant additional permissions
 * (admin; §4.2's elevation re-approval). */
export async function grantExtension(id: string, permissions: string[]): Promise<AddonSummary> {
  return api<AddonSummary>(`/extensions/${encodeURIComponent(id)}/grant`, {
    body: { permissions },
  });
}

// ── Themes (M23's deferred upload surface, delivered M24) ────────

/** One uploaded packaged theme (`GET /themes` data element). */
export interface EngineTheme {
  id: string;
  name: string;
  description: string;
  tokens: Record<string, string>;
  overrides: string[];
}

/** `GET /themes` — the uploaded themes (the presets live in the
 * frontend's own code). */
export async function listThemes(): Promise<{ data: EngineTheme[] }> {
  return api<{ data: EngineTheme[] }>("/themes");
}

/** `POST /themes` — install a packaged theme (`.zip` with
 * `zim.theme.json` at the root; admin). */
export async function uploadTheme(zipBytes: ArrayBuffer): Promise<EngineTheme> {
  const response = await fetch("/api/v1/themes", {
    credentials: "same-origin",
    method: "POST",
    headers: { "Content-Type": "application/zip", Accept: "application/json" },
    body: zipBytes,
  });
  if (!response.ok) {
    throw await parseError(response);
  }
  return (await response.json()) as EngineTheme;
}

/** `DELETE /themes/{id}` — remove an uploaded theme (admin). */
export async function deleteTheme(id: string): Promise<void> {
  return api<void>(`/themes/${encodeURIComponent(id)}`, { method: "DELETE" });
}

// ── The file family (§10.3, M28) ──────────────────────────────────

/** One listed directory entry (`GET .../files`). */
export interface FileEntry {
  name: string;
  is_dir: boolean;
  size: number;
  modified: string | null;
}

/** The listing envelope (`GET .../files[?path=]`). */
export interface FilesListing {
  instance_id: string;
  path: string;
  entries: FileEntry[];
}

/** The rename request body (`POST .../files/rename`). */
export interface RenameFileRequest {
  old_path: string;
  new_path: string;
}

/** The archive request body (`POST .../files/archive`). */
export interface ArchiveFilesRequest {
  paths: string[];
  destination: string;
  format: "tar" | "tar.gz" | "zip";
}

/** The extract request body (`POST .../files/extract`). */
export interface ExtractArchiveRequest {
  archive_path: string;
  destination: string;
}

/** `GET /instances/{id}/files?path={dir}` — list one directory. */
export async function listFiles(instanceId: string, path = "."): Promise<FilesListing> {
  const suffix = path === "." || path === "" ? "" : `?path=${encodeURIComponent(path)}`;
  return api<FilesListing>(`/instances/${encodeURIComponent(instanceId)}/files${suffix}`);
}

/** `GET /instances/{id}/files/content?path={file}` — one file's raw
 * bytes (`application/octet-stream` download). */
export async function downloadFile(instanceId: string, path: string): Promise<ArrayBuffer> {
  const response = await fetch(
    `/api/v1/instances/${encodeURIComponent(instanceId)}/files/content?path=${encodeURIComponent(path)}`,
    { credentials: "same-origin", headers: { Accept: "application/octet-stream" } },
  );
  if (!response.ok) {
    throw await parseError(response);
  }
  return response.arrayBuffer();
}

/** `PUT /instances/{id}/files/content?path={file}` — write one file's
 * bytes (the atomic upload channel; parents created as needed). */
export async function uploadFile(
  instanceId: string,
  path: string,
  bytes: ArrayBuffer | Uint8Array | Blob,
): Promise<{ path: string; size: number }> {
  const response = await fetch(
    `/api/v1/instances/${encodeURIComponent(instanceId)}/files/content?path=${encodeURIComponent(path)}`,
    {
      credentials: "same-origin",
      method: "PUT",
      headers: { "Content-Type": "application/octet-stream", Accept: "application/json" },
      body: bytes as BodyInit,
    },
  );
  if (!response.ok) {
    throw await parseError(response);
  }
  return (await response.json()) as { path: string; size: number };
}

/** `POST /instances/{id}/files/create` — create one file or
 * directory (`is_dir`; a file starts empty). */
export async function createFileEntry(
  instanceId: string,
  path: string,
  isDir: boolean,
): Promise<{ path: string; is_dir: boolean }> {
  return api<{ path: string; is_dir: boolean }>(
    `/instances/${encodeURIComponent(instanceId)}/files/create`,
    { body: { path, is_dir: isDir } },
  );
}

/** `DELETE /instances/{id}/files?path={path}` — delete one file or
 * directory (directories recursively). */
export async function deleteFileEntry(instanceId: string, path: string): Promise<void> {
  const suffix = path === "." || path === "" ? "" : `?path=${encodeURIComponent(path)}`;
  return api<void>(`/instances/${encodeURIComponent(instanceId)}/files${suffix}`, {
    method: "DELETE",
  });
}

/** `POST /instances/{id}/files/rename` — rename or move one path
 * (destination parents created as needed). */
export async function renameFileEntry(
  instanceId: string,
  request: RenameFileRequest,
): Promise<{ old_path: string; new_path: string }> {
  return api<{ old_path: string; new_path: string }>(
    `/instances/${encodeURIComponent(instanceId)}/files/rename`,
    { body: request },
  );
}

/** `POST /instances/{id}/files/archive` — compress an allow-list of
 * paths into one archive inside the instance root. */
export async function archiveFiles(
  instanceId: string,
  request: ArchiveFilesRequest,
): Promise<{ destination: string; format: string; entries: number; bytes: number }> {
  return api<{ destination: string; format: string; entries: number; bytes: number }>(
    `/instances/${encodeURIComponent(instanceId)}/files/archive`,
    { body: request },
  );
}

/** `POST /instances/{id}/files/extract` — extract one archive under
 * a destination inside the instance root. */
export async function extractArchive(
  instanceId: string,
  request: ExtractArchiveRequest,
): Promise<{
  archive_path: string;
  destination: string;
  entries: number;
  bytes: number;
  nested_archives: number;
}> {
  return api<{
    archive_path: string;
    destination: string;
    entries: number;
    bytes: number;
    nested_archives: number;
  }>(`/instances/${encodeURIComponent(instanceId)}/files/extract`, { body: request });
}

/** The join of one directory path and one entry name (root-relative,
 * forward slashes — the §10.3 path grammar). */
export function joinFilePath(directory: string, name: string): string {
  const trimmed = directory === "." || directory === "" ? "" : directory.replace(/\/+$/, "");
  return trimmed === "" ? name : `${trimmed}/${name}`;
}

/** One path's parent, as the listing API expects it (`"."` for the
 * root's children). */
export function parentFilePath(path: string): string {
  const trimmed = path.replace(/\/+$/, "");
  const lastSlash = trimmed.lastIndexOf("/");
  if (lastSlash === -1) {
    return ".";
  }
  return trimmed.slice(0, lastSlash);
}

// ── Backups (§10.5, M29's first management surface) ────────────────

/** One backup row (`GET .../backups` data element). The row is
 * created in progress alongside its durable job and *seals* —
 * `sha256_hash` and `bytes` landing together — when the job
 * succeeds; a failed job leaves the row unsealed forever, which is
 * the honest shape (never a fake sealed row). */
export interface BackupRow {
  id: string;
  instance_id: string;
  job_id: string;
  uuid: string;
  file_name: string;
  bytes: number;
  sha256_hash: string | null;
  locked: boolean;
  completed_at: string | null;
  [key: string]: unknown;
}

/** One durable job row (`GET /jobs/{id}`): the create/restore
 * endpoints answer `{job_id, …}` immediately; this row is where
 * the truth about progress lives (`state`, `last_error`). */
export interface JobRow {
  id: string;
  kind: string;
  instance_id: string;
  state: string;
  created_at: string;
  updated_at: string;
  started_at: string | null;
  completed_at: string | null;
  last_error: string | null;
  cleanup_pending: boolean;
  [key: string]: unknown;
}

/** Whether a job state is terminal — the engine's `JobState`
 * vocabulary (`succeeded`, `failed`, `cancelled`); anything else
 * (`pending`, `in_progress`, `cleanup_pending`) is still moving. */
export function jobIsTerminal(state: string): boolean {
  return state === "succeeded" || state === "failed" || state === "cancelled";
}

/** Format a byte count for humans (binary units, KB-free) — the
 * file manager's convention, shared by the management surfaces. */
export function formatBytes(size: number): string {
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
export async function listBackups(instanceId: string): Promise<BackupRow[]> {
  const body = await api<{ data: BackupRow[] }>(
    `/instances/${encodeURIComponent(instanceId)}/backups`,
  );
  return body.data;
}

/** `POST /instances/{id}/backups` — create a backup: answers `202`'s
 * body (both ids + the job's initial state); the row seals when the
 * durable job completes. */
export async function createBackup(
  instanceId: string,
): Promise<{ job_id: string; backup_id: string; uuid: string; state: string }> {
  return api(`/instances/${encodeURIComponent(instanceId)}/backups`, { method: "POST" });
}

/** `POST .../backups/{bid}/restore` — restore a backup: extraction
 * lands in a staging directory and swaps into place, so a failed
 * restore never corrupts the live data. */
export async function restoreBackup(
  instanceId: string,
  backupId: string,
): Promise<{ job_id: string; backup_id: string; state: string }> {
  return api(
    `/instances/${encodeURIComponent(instanceId)}/backups/${encodeURIComponent(backupId)}/restore`,
    { method: "POST" },
  );
}

/** `DELETE .../backups/{bid}` — delete a backup (its archive and row
 * together). A locked backup refuses with `409 conflict`. */
export async function deleteBackup(instanceId: string, backupId: string): Promise<void> {
  return api(
    `/instances/${encodeURIComponent(instanceId)}/backups/${encodeURIComponent(backupId)}`,
    { method: "DELETE" },
  );
}

/** `GET .../backups/{bid}/download` — the archive's raw bytes
 * (`application/gzip`, `Content-Disposition: attachment`). */
export async function downloadBackup(
  instanceId: string,
  backupId: string,
): Promise<ArrayBuffer> {
  const response = await fetch(
    `/api/v1/instances/${encodeURIComponent(instanceId)}/backups/${encodeURIComponent(backupId)}/download`,
    { credentials: "same-origin", headers: { Accept: "application/gzip" } },
  );
  if (!response.ok) {
    throw await parseError(response);
  }
  return response.arrayBuffer();
}

/** `POST .../backups/{bid}/lock` / `DELETE` — toggle the deletion
 * lock; both answer the updated row. */
export async function setBackupLock(
  instanceId: string,
  backupId: string,
  locked: boolean,
): Promise<BackupRow> {
  return api<BackupRow>(
    `/instances/${encodeURIComponent(instanceId)}/backups/${encodeURIComponent(backupId)}/lock`,
    { method: locked ? "POST" : "DELETE" },
  );
}

/** `GET /jobs/{jid}` — one durable job's row (the create/restore
 * progress oracle). */
export async function getJob(jobId: string): Promise<JobRow> {
  return api(`/jobs/${encodeURIComponent(jobId)}`);
}
