/**
 * `zim://servers/{id}/schedules` — the schedule manager (M29's
 * second management surface, graduating the M22-declared stub).
 *
 * The real §10.6 family over the real engine: list the rows with
 * honest next/last-run times (null is null — the scheduler has not
 * computed a next run yet), create through a form whose cron is
 * structurally pre-checked (the engine stays the authority), pause
 * and resume (`is_active`), trigger now (the honest 409/400
 * refusals of the `only_when_online` gate and a degraded runtime
 * surface as messages, never silent failures), and delete.
 */

import {
  ApiError,
  createSchedule,
  deleteSchedule,
  formatTime,
  getInstance,
  getState,
  isStructurallyValidCron,
  listSchedules,
  patchSchedule,
  runScheduleNow,
  type ScheduleActionBody,
  type ScheduleRow,
} from "./api";
import { ZimStream } from "./stream";
import "./components";
import { initThemeFromStorage } from "./theme";

// The theme applies before anything renders (M23: live, everywhere).
initThemeFromStorage();

/** Where the instance id lives: `/zim/servers/{id}/schedules`. */
function instanceIdFromPath(): string | null {
  const match = window.location.pathname.match(/^\/zim\/servers\/([^/]+)\/schedules\/?$/);
  return match !== null ? decodeURIComponent(match[1]) : null;
}

const nameHeading = document.getElementById("server-name");
const idPill = document.getElementById("server-id");
const dot = document.getElementById("status-dot");
const stateLabel = document.getElementById("state-label");
const listBody = document.getElementById("schedules-list");
const message = document.getElementById("schedules-message");
const newScheduleButton = document.getElementById("btn-new-schedule");
const refreshButton = document.getElementById("btn-refresh");
const scheduleForm = document.getElementById("schedule-form") as HTMLFormElement | null;
const scheduleName = document.getElementById("schedule-name") as HTMLInputElement | null;
const scheduleCron = document.getElementById("schedule-cron") as HTMLInputElement | null;
const scheduleAction = document.getElementById("schedule-action") as HTMLSelectElement | null;
const scheduleCommandField = document.getElementById("schedule-command-field");
const scheduleCommand = document.getElementById("schedule-command") as HTMLInputElement | null;
const scheduleOnline = document.getElementById("schedule-online") as HTMLSelectElement | null;
const authGate = document.getElementById("auth-gate");

/** The instance id, once parsed. */
let instanceId = "";

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

/** One action's one-line description (the table's honest summary). */
function describeAction(action: ScheduleActionBody): string {
  if (action.type === "power") {
    return `power ${action.action}`;
  }
  return `command: ${action.command}`;
}

/** The action select's value → the request body's action. */
function actionFromForm(): ScheduleActionBody | null {
  const value = scheduleAction?.value ?? "";
  if (value.startsWith("power-")) {
    const action = value.slice("power-".length);
    if (action === "start" || action === "stop" || action === "restart" || action === "kill") {
      return { type: "power", action };
    }
    return null;
  }
  const command = scheduleCommand?.value.trim() ?? "";
  if (command === "") {
    return null;
  }
  return { type: "send_command", command };
}

/** One table row for one schedule. */
function scheduleRow(row: ScheduleRow): HTMLTableRowElement {
  const tr = document.createElement("tr");
  tr.dataset.scheduleId = row.id;
  tr.dataset.active = row.is_active ? "true" : "false";

  const nameCell = document.createElement("td");
  nameCell.className = "files-name";
  const nameLine = document.createElement("div");
  nameLine.textContent = row.name;
  nameCell.append(nameLine);
  const actionsLine = document.createElement("div");
  actionsLine.className = "line-dim";
  actionsLine.textContent = row.actions.map(describeAction).join(" · ");
  nameCell.append(actionsLine);

  const cronCell = document.createElement("td");
  cronCell.className = "mgmt-cron mono";
  cronCell.textContent = row.cron;

  const stateCell = document.createElement("td");
  stateCell.className = "mgmt-state";
  const statePill = document.createElement("span");
  statePill.className = row.is_active ? "pill pill-live" : "pill pill-poll";
  statePill.textContent = row.is_active ? "active" : "paused";
  stateCell.append(statePill);
  if (row.only_when_online) {
    const gate = document.createElement("div");
    gate.className = "line-dim";
    gate.textContent = "only while running";
    stateCell.append(gate);
  }

  const runsCell = document.createElement("td");
  runsCell.className = "files-modified";
  const lastLine = document.createElement("div");
  lastLine.textContent = row.last_run_at !== null ? `last: ${formatTime(row.last_run_at)}` : "last: never";
  runsCell.append(lastLine);
  const nextLine = document.createElement("div");
  nextLine.className = "line-dim";
  nextLine.textContent = row.next_run_at !== null ? `next: ${formatTime(row.next_run_at)}` : "next: —";
  runsCell.append(nextLine);

  const actionsCell = document.createElement("td");
  actionsCell.className = "files-actions";
  const runNow = document.createElement("button");
  runNow.type = "button";
  runNow.className = "pill pill-action";
  runNow.textContent = "▶";
  runNow.title = "Trigger this schedule now";
  runNow.setAttribute("aria-label", `run ${row.name} now`);
  runNow.addEventListener("click", () => {
    void runNowAction(row);
  });
  actionsCell.append(runNow);
  const toggle = document.createElement("button");
  toggle.type = "button";
  toggle.className = "pill pill-action";
  toggle.textContent = row.is_active ? "⏸" : "⏵";
  toggle.title = row.is_active ? "Pause (is_active = false)" : "Resume (is_active = true)";
  toggle.setAttribute("aria-label", row.is_active ? `pause ${row.name}` : `resume ${row.name}`);
  toggle.addEventListener("click", () => {
    void toggleActive(row);
  });
  actionsCell.append(toggle);
  const remove = document.createElement("button");
  remove.type = "button";
  remove.className = "pill pill-action";
  remove.textContent = "✕";
  remove.title = `Delete ${row.name}`;
  remove.setAttribute("aria-label", `delete ${row.name}`);
  remove.addEventListener("click", () => {
    void deleteOne(row);
  });
  actionsCell.append(remove);

  tr.append(nameCell, cronCell, stateCell, runsCell, actionsCell);
  return tr;
}

/** Render the whole list. */
function renderListing(rows: ScheduleRow[]): void {
  if (listBody === null) {
    return;
  }
  listBody.replaceChildren();
  if (rows.length === 0) {
    const tr = document.createElement("tr");
    const cell = document.createElement("td");
    cell.colSpan = 5;
    cell.className = "empty";
    cell.textContent = "no schedules yet — create one with ＋ Schedule";
    tr.append(cell);
    listBody.append(tr);
    return;
  }
  for (const row of rows) {
    listBody.append(scheduleRow(row));
  }
}

/** Load and render the list. */
async function refresh(): Promise<void> {
  say("loading…");
  try {
    const rows = await listSchedules(instanceId);
    renderListing(rows);
    say(`${rows.length} schedule${rows.length === 1 ? "" : "s"}`);
  } catch (error) {
    if (error instanceof ApiError && error.isAuth) {
      showAuthGate();
      return;
    }
    renderListing([]);
    say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
  }
}

/** Show the inline create form. */
function showForm(): void {
  if (scheduleForm !== null) {
    scheduleForm.hidden = false;
  }
  scheduleName?.focus();
}

/** Hide the inline create form. */
function hideForm(): void {
  if (scheduleForm !== null) {
    scheduleForm.hidden = true;
  }
}

/** Create a schedule from the form's values. */
async function createFromForm(): Promise<void> {
  const name = scheduleName?.value.trim() ?? "";
  const cron = scheduleCron?.value.trim() ?? "";
  if (name === "") {
    say("a name is required");
    return;
  }
  if (!isStructurallyValidCron(cron)) {
    say("the cron expression needs 5 non-empty fields (minute hour day month weekday)");
    return;
  }
  const action = actionFromForm();
  if (action === null) {
    say(scheduleAction?.value === "send-command" ? "a command is required" : "pick an action");
    return;
  }
  say(`creating ${name}…`);
  try {
    await createSchedule(instanceId, {
      name,
      cron,
      is_active: true,
      only_when_online: scheduleOnline?.value === "yes",
      actions: [action],
    });
    say(`created ${name}`);
    hideForm();
    if (scheduleName !== null) {
      scheduleName.value = "";
    }
    if (scheduleCron !== null) {
      scheduleCron.value = "";
    }
    if (scheduleCommand !== null) {
      scheduleCommand.value = "";
    }
    await refresh();
  } catch (error) {
    say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
  }
}

/** Pause or resume one schedule. */
async function toggleActive(row: ScheduleRow): Promise<void> {
  say(row.is_active ? `pausing ${row.name}…` : `resuming ${row.name}…`);
  try {
    await patchSchedule(instanceId, row.id, { is_active: !row.is_active });
    say(row.is_active ? `paused ${row.name}` : `resumed ${row.name}`);
    await refresh();
  } catch (error) {
    say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
  }
}

/** Trigger one schedule now (the refusals are the contract). */
async function runNowAction(row: ScheduleRow): Promise<void> {
  say(`triggering ${row.name}…`);
  try {
    await runScheduleNow(instanceId, row.id);
    say(`triggered ${row.name}`);
    await refresh();
  } catch (error) {
    if (error instanceof ApiError) {
      say(`${error.code} — ${error.message}`);
    } else {
      say(String(error));
    }
  }
}

/** Delete one schedule (confirm-first). */
async function deleteOne(row: ScheduleRow): Promise<void> {
  if (!window.confirm(`Delete the schedule "${row.name}" (${row.cron})?`)) {
    return;
  }
  say(`deleting ${row.name}…`);
  try {
    await deleteSchedule(instanceId, row.id);
    say(`deleted ${row.name}`);
    await refresh();
  } catch (error) {
    say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
  }
}

function showAuthGate(): void {
  if (authGate !== null) {
    authGate.hidden = false;
  }
}

function wireToolbar(): void {
  newScheduleButton?.addEventListener("click", showForm);
  refreshButton?.addEventListener("click", () => {
    void refresh();
  });
  scheduleAction?.addEventListener("change", () => {
    if (scheduleCommandField !== null) {
      scheduleCommandField.hidden = scheduleAction.value !== "send-command";
    }
  });
  scheduleForm?.addEventListener("submit", (event) => {
    event.preventDefault();
    void createFromForm();
  });
  document.getElementById("schedule-cancel")?.addEventListener("click", hideForm);
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
    } else {
      say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
    }
  }
  if (nameHeading !== null) {
    nameHeading.textContent = `${instanceName} — schedules`;
  }
  document.title = `ZIM — ${instanceName} schedules`;

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
