/**
 * `zim://servers/{id}/backups` — the backup manager (M29's first
 * management surface, graduating the M22-declared stub).
 *
 * The real §10.5 family over the real engine: list the rows with
 * honest metadata (an unsealed row says "sealing" — a failed job
 * never fakes a sealed archive), back up now (the durable job's
 * state is followed live), restore (staging-directory swap, so a
 * failed restore never corrupts the live tree), download, the
 * deletion lock, and delete. Destructive actions confirm; every
 * mutation is an explicit user action; errors surface as the
 * structured `error.message` text, never a silent failure.
 */
import { ApiError, createBackup, deleteBackup, downloadBackup, formatBytes, formatTime, getInstance, getJob, getState, jobIsTerminal, listBackups, restoreBackup, setBackupLock, } from "./api.js";
import { ZimStream } from "./stream.js";
import "./components.js";
import { initThemeFromStorage } from "./theme.js";
// The theme applies before anything renders (M23: live, everywhere).
initThemeFromStorage();
/** How long a job poll keeps asking before giving up (the honest
 * timeout — the row keeps whatever state it truly has). */
const JOB_POLL_TIMEOUT_MS = 120000;
/** How often a job poll re-reads the row. */
const JOB_POLL_INTERVAL_MS = 500;
/** Where the instance id lives: `/zim/servers/{id}/backups`. */
function instanceIdFromPath() {
    const match = window.location.pathname.match(/^\/zim\/servers\/([^/]+)\/backups\/?$/);
    return match !== null ? decodeURIComponent(match[1]) : null;
}
const nameHeading = document.getElementById("server-name");
const idPill = document.getElementById("server-id");
const dot = document.getElementById("status-dot");
const stateLabel = document.getElementById("state-label");
const listBody = document.getElementById("backups-list");
const message = document.getElementById("backups-message");
const backupNowButton = document.getElementById("btn-backup-now");
const refreshButton = document.getElementById("btn-refresh");
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
/** One table row for one backup row. Unsealed rows (their job has
 * not succeeded yet) render the honest "sealing" state instead of
 * a zero-size archive. */
function backupRow(row) {
    const tr = document.createElement("tr");
    tr.dataset.backupId = row.id;
    tr.dataset.locked = row.locked ? "true" : "false";
    tr.dataset.sealed = row.sha256_hash !== null ? "true" : "false";
    const nameCell = document.createElement("td");
    nameCell.className = "files-name";
    const nameLine = document.createElement("div");
    nameLine.textContent = row.file_name;
    nameCell.append(nameLine);
    const uuidLine = document.createElement("div");
    uuidLine.className = "line-dim mono";
    uuidLine.textContent = row.uuid;
    nameCell.append(uuidLine);
    const sizeCell = document.createElement("td");
    sizeCell.className = "files-size mono";
    sizeCell.textContent = row.sha256_hash !== null && row.bytes > 0 ? formatBytes(row.bytes) : "—";
    const checksumCell = document.createElement("td");
    checksumCell.className = "mgmt-checksum mono";
    if (row.sha256_hash !== null) {
        checksumCell.textContent = `${row.sha256_hash.slice(0, 12)}…`;
        checksumCell.title = row.sha256_hash;
    }
    else {
        const sealing = document.createElement("span");
        sealing.className = "pill pill-poll";
        sealing.textContent = "sealing";
        sealing.title = "the durable job has not completed yet";
        checksumCell.append(sealing);
    }
    const sealedCell = document.createElement("td");
    sealedCell.className = "files-modified";
    sealedCell.textContent = row.completed_at !== null ? formatTime(row.completed_at) : "—";
    const lockCell = document.createElement("td");
    lockCell.className = "mgmt-locked";
    const lockPill = document.createElement("span");
    lockPill.className = row.locked ? "pill pill-live" : "pill pill-muted";
    lockPill.textContent = row.locked ? "locked" : "unlocked";
    lockPill.title = row.locked
        ? "locked backups refuse deletion until unlocked"
        : "unlocked — deletion is allowed";
    lockCell.append(lockPill);
    const actionsCell = document.createElement("td");
    actionsCell.className = "files-actions";
    if (row.sha256_hash !== null) {
        const download = document.createElement("button");
        download.type = "button";
        download.className = "pill pill-action";
        download.textContent = "⤓";
        download.title = `Download ${row.file_name}`;
        download.setAttribute("aria-label", `download ${row.file_name}`);
        download.addEventListener("click", () => {
            void downloadArchive(row);
        });
        actionsCell.append(download);
        const restore = document.createElement("button");
        restore.type = "button";
        restore.className = "pill pill-action";
        restore.textContent = "⤺";
        restore.title = `Restore ${row.file_name}`;
        restore.setAttribute("aria-label", `restore ${row.file_name}`);
        restore.addEventListener("click", () => {
            void restoreArchive(row);
        });
        actionsCell.append(restore);
    }
    const lockToggle = document.createElement("button");
    lockToggle.type = "button";
    lockToggle.className = "pill pill-action";
    lockToggle.textContent = row.locked ? "🔓" : "🔒";
    lockToggle.title = row.locked ? "Unlock (allow deletion)" : "Lock (refuse deletion)";
    lockToggle.setAttribute("aria-label", row.locked ? `unlock ${row.file_name}` : `lock ${row.file_name}`);
    lockToggle.addEventListener("click", () => {
        void toggleLock(row);
    });
    actionsCell.append(lockToggle);
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "pill pill-action";
    remove.textContent = "✕";
    remove.title = `Delete ${row.file_name}`;
    remove.setAttribute("aria-label", `delete ${row.file_name}`);
    remove.addEventListener("click", () => {
        void deleteArchive(row);
    });
    actionsCell.append(remove);
    tr.append(nameCell, sizeCell, checksumCell, sealedCell, lockCell, actionsCell);
    return tr;
}
/** Render the whole list (newest first: the engine lists in row
 * order, and the freshest backup is the one the operator watches). */
function renderListing(rows) {
    if (listBody === null) {
        return;
    }
    listBody.replaceChildren();
    if (rows.length === 0) {
        const tr = document.createElement("tr");
        const cell = document.createElement("td");
        cell.colSpan = 6;
        cell.className = "empty";
        cell.textContent = "no backups yet — create one with Back up now";
        tr.append(cell);
        listBody.append(tr);
        return;
    }
    const sorted = [...rows].sort((a, b) => {
        const aTime = a.completed_at ?? "";
        const bTime = b.completed_at ?? "";
        if (aTime !== bTime) {
            return aTime < bTime ? 1 : -1;
        }
        return a.id < b.id ? 1 : -1;
    });
    for (const row of sorted) {
        listBody.append(backupRow(row));
    }
}
/** Load and render the list. */
async function refresh() {
    say("loading…");
    try {
        const rows = await listBackups(instanceId);
        renderListing(rows);
        say(`${rows.length} backup${rows.length === 1 ? "" : "s"}`);
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
/** Follow one durable job until it goes terminal (or the honest
 * timeout); the status line narrates the state. */
async function followJob(jobId, what) {
    const deadline = Date.now() + JOB_POLL_TIMEOUT_MS;
    for (;;) {
        const job = await getJob(jobId);
        if (jobIsTerminal(job.state)) {
            return job;
        }
        say(`${what}: job ${job.id.slice(0, 8)} is ${job.state.replace("_", " ")}…`);
        if (Date.now() >= deadline) {
            return job;
        }
        await new Promise((resolve) => {
            setTimeout(resolve, JOB_POLL_INTERVAL_MS);
        });
    }
}
/** Create a backup now, then follow its job to the truth. */
async function backupNow() {
    if (backupNowButton !== null) {
        backupNowButton.disabled = true;
    }
    say("creating a backup…");
    try {
        const created = await createBackup(instanceId);
        const job = await followJob(created.job_id, "backup");
        if (job.state === "succeeded") {
            say(`backup sealed (job ${job.id.slice(0, 8)})`);
        }
        else if (jobIsTerminal(job.state)) {
            say(`backup job ${job.state.replace("_", " ")}${job.last_error !== null ? ` — ${job.last_error}` : ""}`);
        }
        else {
            say(`backup job still ${job.state.replace("_", " ")} — refresh to follow it`);
        }
        await refresh();
    }
    catch (error) {
        say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
    }
    finally {
        if (backupNowButton !== null) {
            backupNowButton.disabled = false;
        }
    }
}
/** Restore one backup (confirm-first: the swap replaces the live
 * tree when the job completes). */
async function restoreArchive(row) {
    const question = `Restore "${row.file_name}"? The current data is replaced when the restore job completes; ` +
        "a failed restore never corrupts the live tree (it swaps from a staging directory).";
    if (!window.confirm(question)) {
        return;
    }
    say(`restoring ${row.file_name}…`);
    try {
        const started = await restoreBackup(instanceId, row.id);
        const job = await followJob(started.job_id, "restore");
        if (job.state === "succeeded") {
            say(`restored ${row.file_name} (job ${job.id.slice(0, 8)})`);
        }
        else if (jobIsTerminal(job.state)) {
            say(`restore job ${job.state.replace("_", " ")}${job.last_error !== null ? ` — ${job.last_error}` : ""}`);
        }
        else {
            say(`restore job still ${job.state.replace("_", " ")} — refresh to follow it`);
        }
    }
    catch (error) {
        say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
    }
}
/** Download one archive as a real browser download. */
async function downloadArchive(row) {
    say(`downloading ${row.file_name}…`);
    try {
        const bytes = await downloadBackup(instanceId, row.id);
        const blob = new Blob([bytes], { type: "application/gzip" });
        const url = URL.createObjectURL(blob);
        const anchor = document.createElement("a");
        anchor.href = url;
        anchor.download = row.file_name;
        anchor.click();
        URL.revokeObjectURL(url);
        say(`downloaded ${row.file_name} (${formatBytes(bytes.byteLength)})`);
    }
    catch (error) {
        say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
    }
}
/** Toggle the deletion lock. */
async function toggleLock(row) {
    say(row.locked ? `unlocking ${row.file_name}…` : `locking ${row.file_name}…`);
    try {
        await setBackupLock(instanceId, row.id, !row.locked);
        say(row.locked ? `unlocked ${row.file_name}` : `locked ${row.file_name}`);
        await refresh();
    }
    catch (error) {
        say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
    }
}
/** Delete one backup (confirm-first; a locked backup's 409 refusal
 * is surfaced as the honest message, never swallowed). */
async function deleteArchive(row) {
    if (!window.confirm(`Delete the backup "${row.file_name}"? Its archive disappears for good.`)) {
        return;
    }
    say(`deleting ${row.file_name}…`);
    try {
        await deleteBackup(instanceId, row.id);
        say(`deleted ${row.file_name}`);
        await refresh();
    }
    catch (error) {
        if (error instanceof ApiError && error.status === 409) {
            say(`refused — ${row.file_name} is locked against deletion; unlock it first`);
        }
        else {
            say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
        }
    }
}
function showAuthGate() {
    if (authGate !== null) {
        authGate.hidden = false;
    }
}
function wireToolbar() {
    backupNowButton?.addEventListener("click", () => {
        void backupNow();
    });
    refreshButton?.addEventListener("click", () => {
        void refresh();
    });
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
        nameHeading.textContent = `${instanceName} — backups`;
    }
    document.title = `ZIM — ${instanceName} backups`;
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
