/**
 * `zim://servers/{id}/metrics` — the metrics surface (M30, the last
 * declared stub graduating to its own page).
 *
 * The durable series over the engine's telemetry store: one chart
 * per family (CPU, memory, network, processes) rendered from
 * `GET /instances/{id}/metrics/history`, a window switcher over the
 * §10.3a vocabulary, and a live strip driven by the §11 stream
 * (CPU% from consecutive sample deltas, memory and pids as gauges —
 * the wire event carries no network counters, so network lives on
 * the history charts alone, honestly).
 *
 * Unknown is a gap, never a zero line: a chart whose series is all
 * `null` says "nothing reported" instead of drawing an idle-flat
 * lie. View-only like every page: no lifecycle API is called on
 * load (BROWSER_INTEGRATION.md §6).
 */
import { ApiError, formatBytes, formatRate, getInstance, getState, METRICS_WINDOWS, metricsHistory, } from "./api.js";
import { buildChartModel, drawableRuns, gridlineValues, liveCpuPercent } from "./charts.js";
import { ZimStream } from "./stream.js";
import "./components.js";
import { initThemeFromStorage } from "./theme.js";
// The theme applies before anything renders (M23: live, everywhere).
initThemeFromStorage();
/** How often the charts re-read the history while the tab shows. */
const AUTO_REFRESH_MS = 15000;
/** Where the instance id lives: `/zim/servers/{id}/metrics`. */
function instanceIdFromPath() {
    const match = window.location.pathname.match(/^\/zim\/servers\/([^/]+)\/metrics\/?$/);
    return match !== null ? decodeURIComponent(match[1]) : null;
}
const nameHeading = document.getElementById("server-name");
const idPill = document.getElementById("server-id");
const dot = document.getElementById("status-dot");
const stateLabel = document.getElementById("state-label");
const message = document.getElementById("metrics-message");
const emptyNote = document.getElementById("metrics-empty");
const windowBar = document.getElementById("window-bar");
const refreshButton = document.getElementById("btn-refresh");
const authGate = document.getElementById("auth-gate");
const liveCpu = document.getElementById("live-cpu");
const liveMemory = document.getElementById("live-memory");
const livePids = document.getElementById("live-pids");
const summaryLine = document.getElementById("summary-line");
/** The instance id, once parsed. */
let instanceId = "";
/** The selected window (the §10.3a vocabulary; default 1h). */
let selectedWindow = "1h";
/** The stream's last resource-usage sample (for CPU deltas). */
let lastUsage = null;
function applyStatus(status) {
    if (dot !== null) {
        dot.setAttribute("status", status);
        dot.setAttribute("data-label", status);
    }
    if (stateLabel !== null) {
        stateLabel.textContent = `state: ${status}`;
    }
}
function say(text) {
    if (message !== null) {
        message.textContent = text;
    }
}
function showAuthGate() {
    if (authGate !== null) {
        authGate.hidden = false;
    }
}
/** One series extracted from the history points (null-safe). */
function seriesValues(points, key) {
    return points.map((point) => {
        const value = point[key];
        return typeof value === "number" && Number.isFinite(value) ? value : null;
    });
}
/** Format one y-axis tick for humans. */
function formatTick(value) {
    if (value >= 1024) {
        return formatBytes(value);
    }
    if (Number.isInteger(value)) {
        return String(value);
    }
    return value.toFixed(1);
}
/** Draw one chart card's canvas: gridlines, the reference line, and
 * the series as contiguous runs (gaps where unknown). The canvas
 * reports its known-point count through `data-points` so tests (and
 * the curious) can assert without scraping pixels. */
function drawChart(canvasId, options) {
    const canvas = document.getElementById(canvasId);
    if (canvas === null) {
        return;
    }
    const context = canvas.getContext("2d");
    if (context === null) {
        return;
    }
    const { values, width, height, label, known } = options;
    canvas.dataset.points = String(known);
    canvas.dataset.label = label;
    // Hi-DPI: the backing store scales, the geometry stays CSS-pixel.
    const ratio = window.devicePixelRatio || 1;
    canvas.width = Math.max(Math.round(width * ratio), 1);
    canvas.height = Math.max(Math.round(height * ratio), 1);
    canvas.style.width = `${width}px`;
    canvas.style.height = `${height}px`;
    context.setTransform(ratio, 0, 0, ratio, 0, 0);
    context.clearRect(0, 0, width, height);
    const styles = window.getComputedStyle(document.documentElement);
    const colorGrid = styles.getPropertyValue("--color-surface-2") || "#24282f";
    const colorText = styles.getPropertyValue("--color-text-secondary") || "#9aa1ac";
    const colorAccent = styles.getPropertyValue("--color-accent") || "#3b82f6";
    const colorSuccess = styles.getPropertyValue("--color-success") || "#22c55e";
    const colorWarning = styles.getPropertyValue("--color-warning") || "#f59e0b";
    const padding = 28;
    const model = buildChartModel(values, width, height, padding, options.yMax);
    // Gridlines with tick labels (bottom = 0).
    for (const tick of gridlineValues(model.yMax, 4)) {
        const y = padding + (height - padding * 2) * (1 - (model.yMax > 0 ? tick / model.yMax : 0));
        context.strokeStyle = colorGrid;
        context.lineWidth = 1;
        context.beginPath();
        context.moveTo(padding, y);
        context.lineTo(width - padding / 2, y);
        context.stroke();
        context.fillStyle = colorText;
        context.font = "10px system-ui, sans-serif";
        context.textAlign = "left";
        context.fillText(formatTick(tick), 2, y + 3);
    }
    // The dashed reference series (the memory limit).
    if (options.reference !== undefined && model.yMax > 0) {
        const refModel = buildChartModel(options.reference, width, height, padding, model.yMax);
        let drawing = false;
        context.strokeStyle = colorWarning;
        context.setLineDash([4, 3]);
        context.lineWidth = 1;
        context.beginPath();
        for (const vertex of refModel.vertices) {
            if (vertex.y === null) {
                drawing = false;
                continue;
            }
            if (drawing) {
                context.lineTo(vertex.x, vertex.y);
            }
            else {
                context.moveTo(vertex.x, vertex.y);
                drawing = true;
            }
        }
        context.stroke();
        context.setLineDash([]);
    }
    // The series: contiguous runs only — unknown breaks the line.
    context.strokeStyle = colorAccent;
    context.lineWidth = 1.5;
    context.lineJoin = "round";
    for (const run of drawableRuns(model.vertices)) {
        context.beginPath();
        let started = false;
        for (const index of run) {
            const vertex = model.vertices[index];
            if (vertex.y === null) {
                started = false;
                continue;
            }
            if (started) {
                context.lineTo(vertex.x, vertex.y);
            }
            else {
                context.moveTo(vertex.x, vertex.y);
                started = true;
            }
        }
        context.stroke();
    }
    // The card label.
    context.fillStyle = colorText;
    context.font = "11px system-ui, sans-serif";
    context.textAlign = "left";
    context.fillText(label, padding, 12);
    // The peak marker (top-right).
    const peak = values.reduce((acc, value) => (value !== null && (acc === null || value > acc) ? value : acc), null);
    if (peak !== null) {
        context.textAlign = "right";
        context.fillStyle = colorSuccess;
        context.fillText(`peak ${formatTick(peak)}`, width - 4, 12);
    }
}
/** Render one loaded history envelope into the four charts + the
 * summary strip + the honest empty state. */
function renderHistory(history) {
    const points = history.points;
    if (emptyNote !== null) {
        emptyNote.hidden = points.length > 0;
    }
    const cpuValues = seriesValues(points, "cpu_percent");
    const memoryValues = seriesValues(points, "memory_usage_bytes");
    const limitValues = seriesValues(points, "memory_limit_bytes");
    const rxValues = seriesValues(points, "network_rx_bytes_per_sec");
    const txValues = seriesValues(points, "network_tx_bytes_per_sec");
    const pidValues = seriesValues(points, "pids_current");
    const width = Math.max((document.getElementById("charts-grid")?.clientWidth ?? 480) / 2 - 12, 240);
    const height = 140;
    drawChart("chart-cpu", {
        values: cpuValues,
        width,
        height,
        label: "CPU (% of host)",
        known: cpuValues.filter((value) => value !== null).length,
        yMax: 100,
    });
    drawChart("chart-memory", {
        values: memoryValues,
        reference: limitValues,
        width,
        height,
        label: "Memory",
        known: memoryValues.filter((value) => value !== null).length,
    });
    // Network: rx (accent) is the primary series; tx rides the same
    // axis via the reference lane with its own color below.
    drawChart("chart-network", {
        values: rxValues,
        reference: txValues,
        width,
        height,
        label: "Network rx/tx (bytes/s)",
        known: rxValues.filter((value) => value !== null).length,
    });
    drawChart("chart-pids", {
        values: pidValues,
        width,
        height,
        label: "Processes",
        known: pidValues.filter((value) => value !== null).length,
    });
    if (summaryLine !== null) {
        const summary = history.summary;
        const cpu = summary.cpu_percent_avg === null
            ? "—"
            : `${summary.cpu_percent_avg.toFixed(1)}% avg / ${summary.cpu_percent_max?.toFixed(1) ?? "—"}% peak`;
        const memory = summary.memory_bytes_max === null ? "—" : `${formatBytes(summary.memory_bytes_max)} peak`;
        const rx = summary.network_rx_bytes_total;
        const tx = summary.network_tx_bytes_total;
        const rxPeak = summary.network_rx_bytes_per_sec_peak;
        const txPeak = summary.network_tx_bytes_per_sec_peak;
        const network = rx === null && tx === null && rxPeak === null && txPeak === null
            ? "—"
            : `↓ ${formatBytes(rx ?? 0)} / ↑ ${formatBytes(tx ?? 0)} in ${history.window}` +
                (rxPeak !== null || txPeak !== null
                    ? ` (peaks ${formatRate(rxPeak ?? 0)} / ${formatRate(txPeak ?? 0)})`
                    : "");
        summaryLine.textContent = `cpu ${cpu} · memory ${memory} · network ${network} · ${summary.point_count} point(s) at ${history.resolution_seconds}s resolution`;
    }
}
/** Load the selected window's history and render it. */
async function refresh() {
    say(`loading the last ${selectedWindow}…`);
    try {
        const history = await metricsHistory(instanceId, selectedWindow);
        renderHistory(history);
        say(history.points.length > 0
            ? `${history.points.length} point(s) over the last ${selectedWindow} (${history.resolution_seconds}s resolution)`
            : `no samples in the last ${selectedWindow} — the server has not been running, or its runtime reports no stats`);
    }
    catch (error) {
        if (error instanceof ApiError && error.isAuth) {
            showAuthGate();
            return;
        }
        say(error instanceof ApiError ? `${error.code} — ${error.message}` : String(error));
    }
}
/** Build the window switcher (the §10.3a vocabulary). */
function wireWindowBar() {
    if (windowBar === null) {
        return;
    }
    windowBar.replaceChildren();
    for (const window of METRICS_WINDOWS) {
        const button = document.createElement("button");
        button.type = "button";
        button.className = "pill pill-action window-pill";
        button.dataset.window = window;
        button.textContent = window;
        button.setAttribute("aria-pressed", window === selectedWindow ? "true" : "false");
        button.addEventListener("click", () => {
            selectedWindow = window;
            for (const sibling of windowBar?.querySelectorAll(".window-pill") ?? []) {
                sibling.setAttribute("aria-pressed", sibling.dataset.window === window ? "true" : "false");
            }
            void refresh();
        });
        windowBar.append(button);
    }
}
/** The live strip: consecutive stream samples derive CPU%, gauges
 * render memory and pids. */
function applyUsageSample(data) {
    const cpu = liveCpuPercent(lastUsage ?? {}, data);
    lastUsage = data;
    if (liveCpu !== null) {
        liveCpu.textContent = cpu === null ? "cpu —" : `cpu ${cpu.toFixed(1)}%`;
    }
    const memory = typeof data.memory_usage_bytes === "number" ? data.memory_usage_bytes : null;
    const limit = typeof data.memory_limit_bytes === "number" ? data.memory_limit_bytes : null;
    if (liveMemory !== null) {
        liveMemory.textContent =
            memory === null
                ? "memory —"
                : `memory ${formatBytes(memory)}${limit !== null && limit > 0 ? ` / ${formatBytes(limit)}` : ""}`;
    }
    const pids = typeof data.pids_current === "number" ? data.pids_current : null;
    if (livePids !== null) {
        livePids.textContent = pids === null ? "processes —" : `processes ${pids}`;
    }
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
        nameHeading.textContent = `${instanceName} — metrics`;
    }
    document.title = `ZIM — ${instanceName} metrics`;
    try {
        const snapshot = await getState(instanceId);
        applyStatus(snapshot.status);
    }
    catch {
        // The dot stays `unknown` — the honest state.
    }
    wireWindowBar();
    refreshButton?.addEventListener("click", () => {
        void refresh();
    });
    await refresh();
    // The live strip + status dot ride the §11 stream.
    const stream = new ZimStream(instanceId, {
        onEvent: (event) => {
            if (event.type === "instance.state_change" && event.data.to !== undefined) {
                applyStatus(event.data.to);
            }
            else if (event.type === "instance.resource_usage") {
                applyUsageSample(event.data);
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
    // The charts re-read their window while the tab is visible (a
    // backgrounded tab stops asking — the store keeps collecting).
    window.setInterval(() => {
        if (document.visibilityState === "visible") {
            void refresh();
        }
    }, AUTO_REFRESH_MS);
}
void boot();
