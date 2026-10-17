/**
 * `zim://servers/{id}/settings` — the settings surface (M29's
 * fourth and final management surface, graduating the last
 * M22-declared stub).
 *
 * The real §10.3 PATCH family over the real engine, under §8's
 * concurrency discipline: the form prefills from the row (whose
 * `etag` field is the If-Match token), Save sends the full form
 * with that ETag, and a stale form answers 412 — surfaced as the
 * honest "someone else changed this row — reload" message, never
 * a silent overwrite. The identity card renders what PATCH cannot
 * change (uuid, node, egg, owner, timestamps); the danger zone
 * carries the AdminInstance delete under the same gate.
 */

import {
  ApiError,
  bytesToMib,
  deleteInstance,
  formatTime,
  getInstance,
  getState,
  patchInstance,
  settingsFormToPatch,
  type Instance,
  type ResourceLimitsWire,
  type SettingsFormValues,
} from "./api";
import { ZimStream } from "./stream";
import "./components";
import { initThemeFromStorage } from "./theme";

// The theme applies before anything renders (M23: live, everywhere).
initThemeFromStorage();

/** Where the instance id lives: `/zim/servers/{id}/settings`. */
function instanceIdFromPath(): string | null {
  const match = window.location.pathname.match(/^\/zim\/servers\/([^/]+)\/settings\/?$/);
  return match !== null ? decodeURIComponent(match[1]) : null;
}

const nameHeading = document.getElementById("server-name");
const idPill = document.getElementById("server-id");
const dot = document.getElementById("status-dot");
const stateLabel = document.getElementById("state-label");
const etagPill = document.getElementById("etag-pill");
const identityGrid = document.getElementById("identity-grid");
const message = document.getElementById("settings-message");
const reloadButton = document.getElementById("btn-reload");
const deleteButton = document.getElementById("btn-delete");
const settingsForm = document.getElementById("settings-form") as HTMLFormElement | null;
const authGate = document.getElementById("auth-gate");

/** The instance id, once parsed. */
let instanceId = "";

/** The last-read row (the form's prefill source + ETag owner). */
let current: Instance | null = null;

function applyStatus(status: string): void {
  if (dot !== null) {
    dot.setAttribute("status", status);
    dot.setAttribute("data-label", status);
  }
  if (stateLabel !== null) {
    stateLabel.textContent = `state: ${status}`;
  }
}

/** Show one operation's outcome in the toolbar's status line. */
function say(text: string): void {
  if (message !== null) {
    message.textContent = text;
  }
}

/** The row's limits, or the engine's zero-defaults when the row
 * predates a field (honest display, honest prefill). */
function limitsOf(instance: Instance): ResourceLimitsWire {
  const raw = instance.limits as Partial<ResourceLimitsWire> | undefined;
  return {
    memory: typeof raw?.memory === "number" ? raw.memory : 0,
    swap: typeof raw?.swap === "number" ? raw.swap : 0,
    disk: typeof raw?.disk === "number" ? raw.disk : 0,
    io_weight: typeof raw?.io_weight === "number" ? raw.io_weight : 0,
    cpu_millicores: typeof raw?.cpu_millicores === "number" ? raw.cpu_millicores : 0,
    threads: typeof raw?.threads === "string" ? raw.threads : null,
    oom_disabled: raw?.oom_disabled === true,
  };
}

function input(id: string): HTMLInputElement | null {
  return document.getElementById(id) as HTMLInputElement | null;
}

/** Prefill the form from the row (the full-form save's source of
 * truth — every editable field starts filled). */
function fillForm(instance: Instance): void {
  const limits = limitsOf(instance);
  const set = (id: string, value: string): void => {
    const field = input(id);
    if (field !== null) {
      field.value = value;
    }
  };
  const setFlag = (id: string, value: boolean): void => {
    const field = input(id);
    if (field !== null) {
      field.checked = value;
    }
  };
  set("set-name", instance.name);
  set("set-description", instance.description);
  set("set-startup", typeof instance.startup === "string" ? instance.startup : "");
  set("set-image", typeof instance.image === "string" ? instance.image : "");
  set("set-external-id", instance.external_id !== undefined && typeof instance.external_id === "string" ? instance.external_id : "");
  set("set-memory", String(bytesToMib(limits.memory)));
  set("set-swap", String(bytesToMib(limits.swap)));
  set("set-disk", String(bytesToMib(limits.disk)));
  set("set-cpu", String(limits.cpu_millicores));
  set("set-io", String(limits.io_weight));
  set("set-threads", limits.threads ?? "");
  set("set-backup-limit", String(instance.backup_limit ?? 0));
  set("set-database-limit", instance.database_limit !== undefined && instance.database_limit !== null ? String(instance.database_limit) : "");
  set("set-allocation-limit", instance.allocation_limit !== undefined && instance.allocation_limit !== null ? String(instance.allocation_limit) : "");
  setFlag("set-crash", instance.crash_detection_enabled === true);
  setFlag("set-skip-scripts", instance.skip_egg_scripts === true);
  setFlag("set-oom", limits.oom_disabled);
}

/** Read the form into the builder's values shape. */
function readForm(): SettingsFormValues {
  return {
    name: input("set-name")?.value ?? "",
    description: input("set-description")?.value ?? "",
    startup: input("set-startup")?.value ?? "",
    image: input("set-image")?.value ?? "",
    externalId: input("set-external-id")?.value ?? "",
    memoryMib: input("set-memory")?.value ?? "",
    swapMib: input("set-swap")?.value ?? "",
    diskMib: input("set-disk")?.value ?? "",
    cpuMillicores: input("set-cpu")?.value ?? "",
    ioWeight: input("set-io")?.value ?? "",
    threads: input("set-threads")?.value ?? "",
    backupLimit: input("set-backup-limit")?.value ?? "",
    databaseLimit: input("set-database-limit")?.value ?? "",
    allocationLimit: input("set-allocation-limit")?.value ?? "",
    crashDetectionEnabled: input("set-crash")?.checked ?? false,
    skipEggScripts: input("set-skip-scripts")?.checked ?? false,
    oomDisabled: input("set-oom")?.checked ?? false,
  };
}

/** One identity-grid entry (the read-only row facts). */
function identityItem(label: string, value: string): HTMLElement {
  const item = document.createElement("div");
  item.className = "status-item";
  const dt = document.createElement("dt");
  dt.textContent = label;
  const dd = document.createElement("dd");
  dd.textContent = value;
  item.append(dt, dd);
  return item;
}

/** Render the identity card + the ETag pill from the row. */
function renderIdentity(instance: Instance): void {
  if (identityGrid !== null) {
    identityGrid.replaceChildren(
      identityItem("uuid", instance.uuid),
      identityItem("short", instance.uuid_short),
      identityItem("node", instance.node_id),
      identityItem("egg", instance.egg_id),
      identityItem("owner", instance.owner_id),
      identityItem("created", formatTime(instance.created_at)),
      identityItem("updated", formatTime(instance.updated_at)),
    );
  }
  if (etagPill !== null) {
    etagPill.textContent = `etag: ${instance.etag ?? "—"}`;
  }
}

/** Re-read the row, refill the form (the reload path and the
 * post-save path share it — the form always reflects the row). */
async function reload(): Promise<void> {
  say("loading…");
  try {
    const instance = await getInstance(instanceId);
    current = instance;
    fillForm(instance);
    renderIdentity(instance);
    say(`loaded as of ${formatTime(instance.updated_at)}`);
  } catch (error) {
    if (error instanceof ApiError && error.isAuth) {
      showAuthGate();
      return;
    }
    say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
  }
}

/** Save the form under the §8 gate. */
async function save(): Promise<void> {
  if (current === null || current.etag === undefined) {
    say("the row is not loaded yet — Reload first");
    return;
  }
  const request = settingsFormToPatch(readForm());
  if (request === null) {
    say("the form's shape is invalid — check the numbers (whole, non-negative)");
    return;
  }
  say("saving…");
  try {
    const updated = await patchInstance(instanceId, request, current.etag);
    current = updated;
    fillForm(updated);
    renderIdentity(updated);
    say(`saved as of ${formatTime(updated.updated_at)}`);
  } catch (error) {
    if (error instanceof ApiError && error.status === 412) {
      say("412 — someone else changed this row; press Reload and re-apply your edit");
    } else {
      say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
    }
  }
}

/** Delete the instance under the same gate (confirm-first; the
 * page then walks to the server list). */
async function deleteRow(): Promise<void> {
  if (current === null || current.etag === undefined) {
    say("the row is not loaded yet — Reload first");
    return;
  }
  const name = current.name;
  if (!window.confirm(`Delete the instance "${name}"?`)) {
    return;
  }
  if (!window.confirm(`Really delete "${name}"? Schedules, backups, and subusers cascade away with it. This cannot be undone.`)) {
    return;
  }
  say("deleting…");
  try {
    await deleteInstance(instanceId, current.etag);
    window.location.href = "/zim/servers";
  } catch (error) {
    if (error instanceof ApiError && error.status === 412) {
      say("412 — someone else changed this row; press Reload and try again");
    } else {
      say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
    }
  }
}

function showAuthGate(): void {
  if (authGate !== null) {
    authGate.hidden = false;
  }
}

function wireToolbar(): void {
  reloadButton?.addEventListener("click", () => {
    void reload();
  });
  settingsForm?.addEventListener("submit", (event) => {
    event.preventDefault();
    void save();
  });
  deleteButton?.addEventListener("click", () => {
    void deleteRow();
  });
}

async function boot(): Promise<void> {
  const parsed = instanceIdFromPath();
  if (parsed === null) {
    say(`cannot parse a server id from ${window.location.pathname}`);
    return;
  }
  instanceId = parsed;
  if (idPill !== null) {
    idPill.textContent = instanceId;
  }

  let instanceName = instanceId.slice(0, 8);
  try {
    const instance = await getInstance(instanceId);
    instanceName = instance.name;
  } catch (error) {
    if (error instanceof ApiError && error.isAuth) {
      showAuthGate();
      document.addEventListener("zim:authenticated", () => {
        window.location.reload();
      });
      return;
    }
    say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
  }
  if (nameHeading !== null) {
    nameHeading.textContent = `${instanceName} — settings`;
  }
  document.title = `ZIM — ${instanceName} settings`;

  try {
    const snapshot = await getState(instanceId);
    applyStatus(snapshot.status);
  } catch {
    // The dot stays `unknown` — the honest state.
  }

  wireToolbar();
  await reload();

  const stream = new ZimStream(instanceId, {
    onEvent: (event) => {
      if (event.type === "instance.state_change" && event.data.to !== undefined) {
        applyStatus(event.data.to);
      }
    },
    onGap: () => {
      void getState(instanceId)
        .then((snapshot) => {
          applyStatus(snapshot.status);
        })
        .catch(() => undefined);
    },
    onFatal: () => {
      stream.close();
    },
  });
  window.addEventListener("beforeunload", () => {
    stream.close();
  });
}

void boot();
