/**
 * `zim://servers/{id}/databases` — the managed-database surface
 * (M31, the last declared-but-unbuilt per-instance family).
 *
 * The real §10.21 family over the real engine: list the instance's
 * database rows with their connection info, provision a new one
 * (the user-chosen name; the namespace, username, and password are
 * derived server-side), reveal and rotate the credential (the
 * view_password grant's own gate, surfaced honestly when the caller
 * lacks it), and delete. The honest boundary rides the toolbar: the
 * engine stores and manages the rows; provisioning against a live
 * host is the workloads/database integration's milestone.
 */

import {
  ApiError,
  connectionString,
  createDatabase,
  deleteDatabase,
  type DatabaseHostRow,
  type DatabaseRow,
  getInstance,
  getState,
  listDatabaseHosts,
  listDatabases,
  nameBudget,
  revealDatabasePassword,
  rotateDatabasePassword,
} from "./api";
import { ZimStream } from "./stream";
import "./components";
import { initThemeFromStorage } from "./theme";

// The theme applies before anything renders (M23: live, everywhere).
initThemeFromStorage();

/** Where the instance id lives: `/zim/servers/{id}/databases`. */
function instanceIdFromPath(): string | null {
  const match = window.location.pathname.match(/^\/zim\/servers\/([^/]+)\/databases\/?$/);
  return match !== null ? decodeURIComponent(match[1]) : null;
}

const nameHeading = document.getElementById("server-name");
const idPill = document.getElementById("server-id");
const dot = document.getElementById("status-dot");
const stateLabel = document.getElementById("state-label");
const listBody = document.getElementById("databases-list");
const message = document.getElementById("databases-message");
const addDatabaseButton = document.getElementById("btn-add-database");
const refreshButton = document.getElementById("btn-refresh");
const databaseForm = document.getElementById("database-form") as HTMLFormElement | null;
const nameInput = document.getElementById("database-name") as HTMLInputElement | null;
const remoteInput = document.getElementById("database-remote") as HTMLInputElement | null;
const hostSelect = document.getElementById("database-host") as HTMLSelectElement | null;
const authGate = document.getElementById("auth-gate");

/** The instance id, once parsed. */
let instanceId = "";

/** The instance's short uuid, for the name-budget hint. */
let instanceShort = "";

/** The known hosts, for the create form's selector. */
let hosts: DatabaseHostRow[] = [];

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

/** One table row for one database. */
function databaseRow(row: DatabaseRow): HTMLTableRowElement {
  const tr = document.createElement("tr");
  tr.dataset.databaseId = row.id;

  const nameCell = document.createElement("td");
  nameCell.className = "files-name";
  const nameLine = document.createElement("div");
  nameLine.className = "mono";
  nameLine.textContent = row.name;
  nameCell.append(nameLine);
  const userLine = document.createElement("div");
  userLine.className = "line-dim mono";
  userLine.textContent = row.username;
  nameCell.append(userLine);
  const whenLine = document.createElement("div");
  whenLine.className = "line-dim";
  whenLine.textContent = `created ${row.created_at.slice(0, 10)}`;
  nameCell.append(whenLine);

  const connCell = document.createElement("td");
  connCell.className = "mono";
  const address = connectionString(row);
  connCell.textContent = address ?? "(host unknown)";
  if (address === null) {
    connCell.className = "mono line-dim";
  }
  const capsLine = document.createElement("div");
  capsLine.className = "line-dim";
  capsLine.textContent =
    row.max_connections === 0 ? "unlimited connections" : `max ${row.max_connections} connections`;
  connCell.append(capsLine);

  const remoteCell = document.createElement("td");
  remoteCell.className = "mono";
  remoteCell.textContent = row.remote;
  remoteCell.title = "the allowed connection source (remote)";

  const actionsCell = document.createElement("td");
  actionsCell.className = "files-actions";
  const reveal = document.createElement("button");
  reveal.type = "button";
  reveal.className = "pill pill-action";
  reveal.textContent = "👁";
  reveal.title = "Reveal the password (database.view_password)";
  reveal.setAttribute("aria-label", `reveal the password for ${row.name}`);
  reveal.addEventListener("click", () => {
    void revealOne(row);
  });
  actionsCell.append(reveal);
  const rotate = document.createElement("button");
  rotate.type = "button";
  rotate.className = "pill pill-action";
  rotate.textContent = "⟳";
  rotate.title = "Rotate the password (database.update)";
  rotate.setAttribute("aria-label", `rotate the password for ${row.name}`);
  rotate.addEventListener("click", () => {
    void rotateOne(row);
  });
  actionsCell.append(rotate);
  const remove = document.createElement("button");
  remove.type = "button";
  remove.className = "pill pill-action";
  remove.textContent = "✕";
  remove.title = `Delete ${row.name}`;
  remove.setAttribute("aria-label", `delete ${row.name}`);
  remove.addEventListener("click", () => {
    void removeOne(row);
  });
  actionsCell.append(remove);

  tr.append(nameCell, connCell, remoteCell, actionsCell);
  return tr;
}

/** Render the whole list. */
function renderListing(rows: DatabaseRow[]): void {
  if (listBody === null) {
    return;
  }
  listBody.replaceChildren();
  if (rows.length === 0) {
    const tr = document.createElement("tr");
    const cell = document.createElement("td");
    cell.colSpan = 4;
    cell.className = "empty";
    cell.textContent =
      "no databases yet — provision one with ＋ Database (an admin must register a host first)";
    tr.append(cell);
    listBody.append(tr);
    return;
  }
  for (const row of rows) {
    listBody.append(databaseRow(row));
  }
}

/** Load and render the list (and refresh the host selector's
 * counts — the form stays honest about capacity). */
async function refresh(): Promise<void> {
  say("loading…");
  try {
    const rows = await listDatabases(instanceId);
    renderListing(rows);
    say(`${rows.length} database${rows.length === 1 ? "" : "s"}`);
    await loadHosts();
  } catch (error) {
    if (error instanceof ApiError && error.isAuth) {
      showAuthGate();
      return;
    }
    renderListing([]);
    say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
  }
}

/** Load the host list for the create form (admin-only: a 403 hides
 * the selector but the form still works on “auto”). */
async function loadHosts(): Promise<void> {
  if (hostSelect === null) {
    return;
  }
  try {
    hosts = await listDatabaseHosts();
  } catch {
    hosts = [];
  }
  hostSelect.replaceChildren();
  const auto = document.createElement("option");
  auto.value = "";
  auto.textContent = hosts.length === 0 ? "auto (no hosts registered)" : "auto (node-tied first)";
  hostSelect.append(auto);
  for (const host of hosts) {
    const option = document.createElement("option");
    option.value = host.id;
    const cap =
      host.max_databases === null
        ? ""
        : ` — ${host.databases_count}/${host.max_databases}`;
    option.textContent = `${host.name} (${host.host}:${host.port}${cap})`;
    hostSelect.append(option);
  }
}

/** Provision a database from the form's values. The response carries
 * the one-time password: surface it loudly (a prompt makes it
 * copyable; reads never repeat it without the grant). */
async function createFromForm(): Promise<void> {
  const name = nameInput?.value.trim() ?? "";
  if (name === "") {
    say("a name is required");
    return;
  }
  const remote = remoteInput?.value.trim() ?? "";
  const hostId = hostSelect?.value ?? "";
  say("provisioning…");
  try {
    const row = await createDatabase(instanceId, {
      name,
      ...(remote !== "" ? { remote } : {}),
      ...(hostId !== "" ? { host_id: hostId } : {}),
    });
    window.prompt(
      `Database ${row.name} provisioned.\n\nSave the password now — reads never repeat it without the view_password grant:\n\n${row.password}`,
      row.password,
    );
    say(`provisioned ${row.name} on ${row.connections.host}:${row.connections.port}`);
    hideForm();
    if (nameInput !== null) {
      nameInput.value = "";
    }
    await refresh();
  } catch (error) {
    say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
  }
}

/** Reveal one database's password (the view_password grant's own
 * gate — a 403 says exactly who cannot). */
async function revealOne(row: DatabaseRow): Promise<void> {
  say(`revealing ${row.name}…`);
  try {
    const password = await revealDatabasePassword(instanceId, row.id);
    window.prompt(`Password for ${row.name} (${row.username}):`, password);
    say(`revealed ${row.name}`);
  } catch (error) {
    if (error instanceof ApiError && error.isPermission) {
      say(`reveal refused — ${error.missingPermissions.join(", ") || "view_password"}`);
      return;
    }
    say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
  }
}

/** Rotate one database's password; the answer carries the new value
 * once (the same one-time reveal as create). */
async function rotateOne(row: DatabaseRow): Promise<void> {
  if (!window.confirm(`Rotate ${row.name}'s password? Workloads using the old one stop authenticating.`)) {
    return;
  }
  say(`rotating ${row.name}…`);
  try {
    const password = await rotateDatabasePassword(instanceId, row.id);
    window.prompt(`New password for ${row.name} — save it now:`, password);
    say(`rotated ${row.name}`);
  } catch (error) {
    say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
  }
}

/** Delete one database (confirm-first). */
async function removeOne(row: DatabaseRow): Promise<void> {
  if (!window.confirm(`Delete the database row ${row.name}? The credential goes with it.`)) {
    return;
  }
  say(`deleting ${row.name}…`);
  try {
    await deleteDatabase(instanceId, row.id);
    say(`deleted ${row.name}`);
    await refresh();
  } catch (error) {
    say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
  }
}

function showForm(): void {
  // The name hint states the honest budget for THIS instance.
  const hint = document.getElementById("name-hint");
  if (hint !== null && instanceShort !== "") {
    hint.textContent = `the s${instanceShort}_ namespace is added (${nameBudget(instanceShort)} characters to spend)`;
  }
  if (databaseForm !== null) {
    databaseForm.hidden = false;
  }
  nameInput?.focus();
}

function hideForm(): void {
  if (databaseForm !== null) {
    databaseForm.hidden = true;
  }
}

function showAuthGate(): void {
  if (authGate !== null) {
    authGate.hidden = false;
  }
}

function wireToolbar(): void {
  addDatabaseButton?.addEventListener("click", showForm);
  refreshButton?.addEventListener("click", () => {
    void refresh();
  });
  databaseForm?.addEventListener("submit", (event) => {
    event.preventDefault();
    void createFromForm();
  });
  document.getElementById("database-cancel")?.addEventListener("click", hideForm);
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
    instanceShort = typeof instance.uuid_short === "string" ? instance.uuid_short : "";
  } catch (error) {
    if (error instanceof ApiError && error.isAuth) {
      showAuthGate();
      document.addEventListener("zim:authenticated", () => {
        window.location.reload();
      });
    } else {
      say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
    }
  }
  if (nameHeading !== null) {
    nameHeading.textContent = `${instanceName} — databases`;
  }
  document.title = `ZIM — ${instanceName} databases`;

  try {
    const snapshot = await getState(instanceId);
    applyStatus(snapshot.status);
  } catch {
    // The dot stays `unknown` — the honest state.
  }

  wireToolbar();
  await refresh();

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
