/**
 * `zim://servers/{id}/{surface}` — the per-instance surfaces M22
 * declares ahead of their full features: settings, metrics (the
 * console keeps its own page; files graduated in M28; backups,
 * schedules, and subusers in M29).
 *
 * Each surface renders the real instance header (row + observed
 * state, live over the §11 stream) plus its honest placeholder
 * body: the §3 URL inventory is real and navigable now, and each
 * surface says exactly what it is — a declared surface whose full
 * functionality ships with the milestone that owns it. The metrics
 * surface is further along than the others: it renders the real
 * recent resource-usage samples (`GET /instances/{id}/metrics`).
 *
 * View-only like every page: no lifecycle API is called on load
 * (BROWSER_INTEGRATION.md §6).
 */
import { ApiError, getInstance, getState, metricsSnapshot } from "./api.js";
import { ZimStream } from "./stream.js";
import "./components.js";
import { initThemeFromStorage } from "./theme.js";
// The theme applies before anything renders (M23: live, everywhere).
initThemeFromStorage();
/** Which surface this page is (`/zim/servers/{id}/{surface}`). */
function surfaceFromPath() {
    const match = window.location.pathname.match(/^\/zim\/servers\/([^/]+)\/([a-z]+)\/?$/);
    if (match === null) {
        return null;
    }
    return { instanceId: decodeURIComponent(match[1]), surface: match[2] };
}
/** The honest one-liner per surface (what exists, what waits).
 * The files surface graduated to its own page (M28's
 * `pages/files.html`); backups, schedules, and subusers to their
 * own pages (M29); the stub never sees them, but the notes stay
 * gone rather than lying. */
const SURFACE_NOTES = {
    settings: "Instance configuration ships with a later milestone; power controls live on the console today.",
    metrics: "Live samples below; charts and history ship with a later milestone.",
};
const nameHeading = document.getElementById("server-name");
const idPill = document.getElementById("server-id");
const surfacePill = document.getElementById("surface-name");
const dot = document.getElementById("status-dot");
const stateLabel = document.getElementById("state-label");
const note = document.getElementById("surface-note");
const metricsCard = document.getElementById("metrics-card");
const metricsBody = document.getElementById("metrics-body");
const authGate = document.getElementById("auth-gate");
function applyStatus(status) {
    if (dot !== null) {
        dot.setAttribute("status", status);
        dot.setAttribute("data-label", status);
    }
    if (stateLabel !== null) {
        stateLabel.textContent = `state: ${status}`;
    }
}
function showAuthGate() {
    if (authGate !== null) {
        authGate.hidden = false;
    }
}
async function boot() {
    const where = surfaceFromPath();
    if (where === null) {
        if (note !== null) {
            note.textContent = `cannot parse a server surface from ${window.location.pathname}`;
        }
        return;
    }
    const { instanceId, surface } = where;
    if (idPill !== null) {
        idPill.textContent = instanceId;
    }
    if (surfacePill !== null) {
        surfacePill.textContent = surface;
    }
    if (note !== null) {
        note.textContent = SURFACE_NOTES[surface] ?? "This surface is declared; its full view ships with a later milestone.";
    }
    document.title = `ZIM — ${surface}`;
    const consoleLink = document.getElementById("nav-console");
    if (consoleLink !== null) {
        consoleLink.setAttribute("href", `/zim/servers/${encodeURIComponent(instanceId)}/console`);
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
        else if (note !== null) {
            note.textContent = error instanceof ApiError ? `${error.code} — ${error.message}` : String(error);
        }
    }
    if (nameHeading !== null) {
        nameHeading.textContent = `${instanceName} — ${surface}`;
    }
    document.title = `ZIM — ${instanceName} ${surface}`;
    try {
        const snapshot = await getState(instanceId);
        applyStatus(snapshot.status);
    }
    catch {
        // The dot stays `unknown` — the honest state.
    }
    if (surface === "metrics") {
        await renderMetrics(instanceId);
    }
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
/** The metrics surface's real data: the latest resource-usage
 * sample, humanized. */
async function renderMetrics(instanceId) {
    if (metricsCard === null || metricsBody === null) {
        return;
    }
    try {
        const snapshot = await metricsSnapshot(instanceId);
        const latest = snapshot.samples[snapshot.samples.length - 1];
        metricsBody.replaceChildren();
        if (latest === undefined) {
            const empty = document.createElement("p");
            empty.className = "empty";
            empty.textContent = "no samples yet — the monitor emits them while the server runs";
            metricsBody.append(empty);
        }
        else {
            metricsBody.append(...sampleRows(latest));
        }
        const meta = document.createElement("p");
        meta.className = "console-meta mono";
        meta.textContent = `${snapshot.samples.length} sample(s), generation ${snapshot.last_generation}`;
        metricsBody.append(meta);
        metricsCard.hidden = false;
    }
    catch (error) {
        if (error instanceof ApiError && error.isAuth) {
            showAuthGate();
            return;
        }
        const line = document.createElement("p");
        line.className = "empty";
        line.textContent = error instanceof ApiError ? `${error.code} — ${error.message}` : String(error);
        metricsBody.replaceChildren(line);
        metricsCard.hidden = false;
    }
}
/** Render one §11.1 `instance.resource_usage` sample. */
function sampleRows(sample) {
    const data = sample.data;
    const rows = [];
    const cpu = typeof data.cpu_usage_total_ns === "number" ? data.cpu_usage_total_ns : undefined;
    const sys = typeof data.system_cpu_total_ns === "number" ? data.system_cpu_total_ns : undefined;
    const cpus = typeof data.online_cpus === "number" ? data.online_cpus : undefined;
    const mem = typeof data.memory_usage_bytes === "number" ? data.memory_usage_bytes : undefined;
    const limit = typeof data.memory_limit_bytes === "number" ? data.memory_limit_bytes : undefined;
    const pids = typeof data.pids_current === "number" ? data.pids_current : undefined;
    if (cpu !== undefined && sys !== undefined && sys > 0 && cpus !== undefined && cpus > 0) {
        const share = Math.min((cpu / sys) * cpus * 100, 100 * cpus);
        rows.push(metricRow("CPU", `${share.toFixed(1)}% of ${cpus} core(s)`));
    }
    else if (cpu !== undefined) {
        rows.push(metricRow("CPU", `${(cpu / 1000000).toFixed(0)} ms total`));
    }
    if (mem !== undefined) {
        const of = limit !== undefined ? ` of ${(limit / 1024 ** 3).toFixed(1)} GiB` : "";
        rows.push(metricRow("Memory", `${(mem / 1024 ** 2).toFixed(1)} MiB${of}`));
    }
    if (pids !== undefined) {
        rows.push(metricRow("Processes", String(pids)));
    }
    rows.push(metricRow("Observed", sample.ts));
    return rows;
}
function metricRow(label, value) {
    const row = document.createElement("div");
    row.className = "status-item";
    const dt = document.createElement("dt");
    dt.textContent = label;
    const dd = document.createElement("dd");
    dd.textContent = value;
    row.append(dt, dd);
    return row;
}
void boot();
