/**
 * `zim://servers` — the server list.
 *
 * One HTTP page fetch, then live state updates: one §11 WebSocket
 * per subscribed instance (up to {@link MAX_LIVE_SUBSCRIPTIONS};
 * the rest fall back to 5-second polling). Row state comes from
 * `instance.state_change` events; a `stream.gap` triggers a state
 * re-fetch per §11.3.
 */
import { ApiError, formatTime, getState, listInstances } from "./api";
import { ZimStream } from "./stream";
import "./components";
/** Rows that get a live WebSocket (per-instance streams, §11). */
const MAX_LIVE_SUBSCRIPTIONS = 10;
/** Fallback polling interval for unsubscribed rows. */
const POLL_INTERVAL_MS = 5000;
const rows = document.getElementById("server-rows");
const authGate = document.getElementById("auth-gate");
const liveIndicator = document.getElementById("live-indicator");
const liveRows = new Map();
const pollRows = new Map();
const streams = [];
async function load() {
    let instances;
    try {
        instances = await listInstances();
    }
    catch (error) {
        if (error instanceof ApiError && error.isAuth) {
            if (authGate !== null) {
                authGate.hidden = false;
            }
            renderEmpty("Sign in to list your servers.");
            document.addEventListener("zim:authenticated", () => {
                if (authGate !== null) {
                    authGate.hidden = true;
                }
                void load();
            });
            return;
        }
        renderEmpty(error instanceof ApiError ? error.message : String(error));
        return;
    }
    teardown();
    if (instances.length === 0) {
        renderEmpty("No servers yet — create one via the engine API.");
        return;
    }
    const fragment = document.createDocumentFragment();
    instances.forEach((instance, index) => {
        const cells = buildRow(instance);
        fragment.append(cells.row);
        const row = { instance, ...cells.parts, stream: null };
        if (index < MAX_LIVE_SUBSCRIPTIONS) {
            liveRows.set(instance.id, row);
        }
        else {
            pollRows.set(instance.id, row);
        }
    });
    if (rows !== null) {
        rows.replaceChildren(fragment);
    }
    // Live subscriptions first (the acceptance surface: real-time
    // state changes), polling for the overflow.
    for (const row of liveRows.values()) {
        row.stream = subscribe(row);
        streams.push(row.stream);
    }
    updateIndicator();
    if (pollRows.size > 0) {
        setInterval(() => {
            void pollOnce();
        }, POLL_INTERVAL_MS);
    }
}
function buildRow(instance) {
    const row = document.createElement("tr");
    const statusCell = document.createElement("td");
    const dot = document.createElement("zim-status-dot");
    dot.setAttribute("status", instance.status);
    dot.setAttribute("data-label", instance.status);
    statusCell.append(dot);
    const nameCell = document.createElement("td");
    const link = document.createElement("a");
    link.href = `/zim/servers/${encodeURIComponent(instance.id)}/console`;
    link.textContent = instance.name;
    nameCell.append(link);
    const stateCell = document.createElement("td");
    stateCell.className = "state-label";
    stateCell.textContent = instance.status;
    const updatedCell = document.createElement("td");
    updatedCell.className = "state-label";
    updatedCell.textContent = formatTime(instance.updated_at);
    const openCell = document.createElement("td");
    const open = document.createElement("a");
    open.href = `/zim/servers/${encodeURIComponent(instance.id)}/console`;
    open.textContent = "console →";
    openCell.append(open);
    row.append(statusCell, nameCell, stateCell, updatedCell, openCell);
    return { row, parts: { dot, stateCell, updatedCell } };
}
function subscribe(row) {
    return new ZimStream(row.instance.id, {
        onEvent: (event) => {
            if (event.type === "instance.state_change" && event.data.to !== undefined) {
                applyState(row, event.data.to, event.ts);
            }
        },
        onGap: () => {
            // §11.3: replay could not cover a range — re-fetch over HTTP.
            void refetchState(row);
        },
        onStatus: (status) => {
            if (status === "down" || status === "reconnecting") {
                updateIndicator();
            }
            else {
                updateIndicator();
            }
        },
        onFatal: () => {
            // Permission-denied upgrade or dead engine: poll this row.
            const stream = row.stream;
            row.stream = null;
            if (stream !== null) {
                stream.close();
            }
            liveRows.delete(row.instance.id);
            pollRows.set(row.instance.id, row);
            updateIndicator();
        },
    });
}
function applyState(row, status, ts) {
    row.dot.setAttribute("status", status);
    row.dot.setAttribute("data-label", status);
    row.stateCell.textContent = status;
    row.updatedCell.textContent = formatTime(ts);
}
async function refetchState(row) {
    try {
        const snapshot = await getState(row.instance.id);
        applyState(row, snapshot.status, snapshot.updated_at);
    }
    catch {
        // The instance may have been deleted; leave the last state.
    }
}
async function pollOnce() {
    for (const row of pollRows.values()) {
        await refetchState(row);
    }
}
function updateIndicator() {
    if (liveIndicator === null) {
        return;
    }
    let live = 0;
    for (const row of liveRows.values()) {
        if (row.stream !== null && row.stream.connected) {
            live += 1;
        }
    }
    if (live > 0) {
        liveIndicator.textContent = `live: ${live} stream(s)`;
        liveIndicator.className = "pill pill-live";
    }
    else if (liveRows.size > 0 || pollRows.size > 0) {
        liveIndicator.textContent = pollRows.size > 0 ? "polling" : "reconnecting";
        liveIndicator.className = "pill pill-poll";
    }
    else {
        liveIndicator.textContent = "live: —";
        liveIndicator.className = "pill pill-unknown";
    }
}
function renderEmpty(message) {
    if (rows === null) {
        return;
    }
    const cell = document.createElement("td");
    cell.colSpan = 5;
    cell.className = "empty";
    cell.textContent = message;
    const row = document.createElement("tr");
    row.append(cell);
    rows.replaceChildren(row);
}
function teardown() {
    for (const stream of streams) {
        stream.close();
    }
    streams.length = 0;
    liveRows.clear();
    pollRows.clear();
}
const refreshButton = document.getElementById("refresh");
refreshButton?.addEventListener("click", () => {
    void load();
});
void load();
