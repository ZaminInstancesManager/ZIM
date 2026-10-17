/**
 * `zim://servers/{id}/subusers` — the subuser manager (M29's third
 * management surface, graduating the M22-declared stub).
 *
 * The real §10.7 family over the real engine: list the permission
 * rows joined with the users' public identity, add a subuser by
 * user id with a checkbox form over the engine's real
 * PermissionGrant vocabulary, replace a subuser's grant set, and
 * remove. The owner is implicitly authorized (no row, and the
 * engine refuses adding the caller with 409 — surfaced honestly).
 */
import { ApiError, addSubuser, getInstance, getState, GRANT_VOCABULARY, listSubusers, removeSubuser, updateSubuser, } from "./api.js";
import { ZimStream } from "./stream.js";
import "./components.js";
import { initThemeFromStorage } from "./theme.js";
// The theme applies before anything renders (M23: live, everywhere).
initThemeFromStorage();
/** Where the instance id lives: `/zim/servers/{id}/subusers`. */
function instanceIdFromPath() {
    const match = window.location.pathname.match(/^\/zim\/servers\/([^/]+)\/subusers\/?$/);
    return match !== null ? decodeURIComponent(match[1]) : null;
}
const nameHeading = document.getElementById("server-name");
const idPill = document.getElementById("server-id");
const dot = document.getElementById("status-dot");
const stateLabel = document.getElementById("state-label");
const listBody = document.getElementById("subusers-list");
const message = document.getElementById("subusers-message");
const addSubuserButton = document.getElementById("btn-add-subuser");
const refreshButton = document.getElementById("btn-refresh");
const subuserForm = document.getElementById("subuser-form");
const subuserIdInput = document.getElementById("subuser-id");
const grantsBox = document.getElementById("subuser-grants");
const authGate = document.getElementById("auth-gate");
/** The instance id, once parsed. */
let instanceId = "";
function applyStatus(status) {
    if (dot !== null) {
        dot.setAttribute("status", status);
        dot.setAttribute("data-label", status);
    }
    if (stateLabel !== null) {
        stateLabel.textContent = `state: ${status}`;
    }
}
/** Show one operation's outcome in the toolbar's status line. */
function say(text) {
    if (message !== null) {
        message.textContent = text;
    }
}
/** One grant chip (a label-pill; the count truncates politely with
 * the full set on hover). */
function grantChips(grants) {
    if (grants.length === 0) {
        const none = document.createElement("span");
        none.className = "pill pill-muted";
        none.textContent = "no grants";
        return [none];
    }
    return grants.map((grant) => {
        const chip = document.createElement("span");
        chip.className = "pill pill-muted mono";
        chip.textContent = grant;
        return chip;
    });
}
/** One table row for one subuser. */
function subuserRow(row) {
    const tr = document.createElement("tr");
    tr.dataset.userId = row.user_id;
    const userCell = document.createElement("td");
    userCell.className = "files-name";
    const nameLine = document.createElement("div");
    nameLine.textContent = row.username;
    userCell.append(nameLine);
    const emailLine = document.createElement("div");
    emailLine.className = "line-dim";
    emailLine.textContent = row.email;
    userCell.append(emailLine);
    const idLine = document.createElement("div");
    idLine.className = "line-dim mono";
    idLine.textContent = row.user_id;
    userCell.append(idLine);
    const grantsCell = document.createElement("td");
    grantsCell.className = "mgmt-grants";
    const chips = document.createElement("div");
    chips.className = "chip-row";
    chips.title = grantsSummary(row.grants);
    chips.append(...grantChips(row.grants));
    grantsCell.append(chips);
    const count = document.createElement("div");
    count.className = "line-dim";
    count.textContent = `${row.grants.length} grant${row.grants.length === 1 ? "" : "s"}`;
    grantsCell.append(count);
    const actionsCell = document.createElement("td");
    actionsCell.className = "files-actions";
    const edit = document.createElement("button");
    edit.type = "button";
    edit.className = "pill pill-action";
    edit.textContent = "✎";
    edit.title = `Replace ${row.username}'s grants`;
    edit.setAttribute("aria-label", `edit grants for ${row.username}`);
    edit.addEventListener("click", () => {
        void editGrants(row);
    });
    actionsCell.append(edit);
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "pill pill-action";
    remove.textContent = "✕";
    remove.title = `Remove ${row.username}`;
    remove.setAttribute("aria-label", `remove ${row.username}`);
    remove.addEventListener("click", () => {
        void removeOne(row);
    });
    actionsCell.append(remove);
    tr.append(userCell, grantsCell, actionsCell);
    return tr;
}
/** The hover summary for a grant set (grouped lines). */
function grantsSummary(grants) {
    if (grants.length === 0) {
        return "no grants";
    }
    return grants.join("\n");
}
/** Render the whole list. */
function renderListing(rows) {
    if (listBody === null) {
        return;
    }
    listBody.replaceChildren();
    if (rows.length === 0) {
        const tr = document.createElement("tr");
        const cell = document.createElement("td");
        cell.colSpan = 3;
        cell.className = "empty";
        cell.textContent = "no subusers yet — the owner is implicitly authorized; add teammates with ＋ Subuser";
        tr.append(cell);
        listBody.append(tr);
        return;
    }
    for (const row of rows) {
        listBody.append(subuserRow(row));
    }
}
/** Load and render the list. */
async function refresh() {
    say("loading…");
    try {
        const rows = await listSubusers(instanceId);
        renderListing(rows);
        say(`${rows.length} subuser${rows.length === 1 ? "" : "s"}`);
    }
    catch (error) {
        if (error instanceof ApiError && error.isAuth) {
            showAuthGate();
            return;
        }
        renderListing([]);
        say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
    }
}
/** Build the grant checkboxes (one label + box per grant, grouped
 * by the vocabulary's own grouping). */
function buildGrantCheckboxes(checked = []) {
    if (grantsBox === null) {
        return;
    }
    grantsBox.replaceChildren();
    for (const group of GRANT_VOCABULARY) {
        const groupLabel = document.createElement("div");
        groupLabel.className = "line-dim";
        groupLabel.style.width = "100%";
        groupLabel.textContent = group.group;
        grantsBox.append(groupLabel);
        for (const grant of group.grants) {
            const label = document.createElement("label");
            label.className = "pill pill-muted mono";
            label.style.cursor = "pointer";
            const box = document.createElement("input");
            box.type = "checkbox";
            box.value = grant;
            box.checked = checked.includes(grant);
            box.dataset.grant = grant;
            box.setAttribute("aria-label", grant);
            label.append(box, ` ${grant}`);
            grantsBox.append(label);
        }
    }
}
/** The checked grants in the form. */
function checkedGrants() {
    if (grantsBox === null) {
        return [];
    }
    const grants = [];
    for (const box of grantsBox.querySelectorAll("input[type=checkbox]:checked")) {
        grants.push(box.value);
    }
    return grants;
}
/** Show the inline add form (fresh checkboxes). */
function showForm() {
    buildGrantCheckboxes();
    if (subuserForm !== null) {
        subuserForm.hidden = false;
    }
    subuserIdInput?.focus();
}
/** Hide the inline form. */
function hideForm() {
    if (subuserForm !== null) {
        subuserForm.hidden = true;
    }
}
/** Add a subuser from the form's values. */
async function createFromForm() {
    const userId = subuserIdInput?.value.trim() ?? "";
    if (userId === "") {
        say("a user id is required");
        return;
    }
    const grants = checkedGrants();
    say(`adding ${userId.slice(0, 8)}…`);
    try {
        const row = await addSubuser(instanceId, userId, grants);
        say(`added ${row.username}`);
        hideForm();
        if (subuserIdInput !== null) {
            subuserIdInput.value = "";
        }
        await refresh();
    }
    catch (error) {
        say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
    }
}
/** Replace one subuser's grant set (prompt-driven grant list, the
 * engine validates against the vocabulary). */
async function editGrants(row) {
    const next = window.prompt(`Replace ${row.username}'s grants (comma-separated, e.g. "control.start,file.read"):`, row.grants.join(","));
    if (next === null) {
        return;
    }
    const grants = next
        .split(",")
        .map((grant) => grant.trim())
        .filter((grant) => grant !== "");
    say(`updating ${row.username}'s grants…`);
    try {
        await updateSubuser(instanceId, row.user_id, grants);
        say(`updated ${row.username} (${grants.length} grant${grants.length === 1 ? "" : "s"})`);
        await refresh();
    }
    catch (error) {
        say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
    }
}
/** Remove one subuser (confirm-first). */
async function removeOne(row) {
    if (!window.confirm(`Remove ${row.username} (${row.email}) from this server?`)) {
        return;
    }
    say(`removing ${row.username}…`);
    try {
        await removeSubuser(instanceId, row.user_id);
        say(`removed ${row.username}`);
        await refresh();
    }
    catch (error) {
        say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
    }
}
function showAuthGate() {
    if (authGate !== null) {
        authGate.hidden = false;
    }
}
function wireToolbar() {
    addSubuserButton?.addEventListener("click", showForm);
    refreshButton?.addEventListener("click", () => {
        void refresh();
    });
    subuserForm?.addEventListener("submit", (event) => {
        event.preventDefault();
        void createFromForm();
    });
    document.getElementById("subuser-cancel")?.addEventListener("click", hideForm);
}
async function boot() {
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
    }
    catch (error) {
        if (error instanceof ApiError && error.isAuth) {
            showAuthGate();
            document.addEventListener("zim:authenticated", () => {
                window.location.reload();
            });
        }
        else {
            say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
        }
    }
    if (nameHeading !== null) {
        nameHeading.textContent = `${instanceName} — subusers`;
    }
    document.title = `ZIM — ${instanceName} subusers`;
    try {
        const snapshot = await getState(instanceId);
        applyStatus(snapshot.status);
    }
    catch {
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
