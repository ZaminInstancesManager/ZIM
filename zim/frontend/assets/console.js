/**
 * `zim://servers/{id}/console` — the server console.
 *
 * The §10.x data surfaces (row, observed state, buffered console)
 * followed by the live §11 stream: state changes re-render the power
 * controls, console output appends to the log, and input is sent via
 * the HTTP endpoint (the v1 WS contract has no client→server frames).
 */
import { ApiError, consoleBuffer, consoleInput, getInstance, getState, power } from "./api.js";
import { ZimStream } from "./stream.js";
import "./components.js";
import { initThemeFromStorage } from "./theme.js";
import { addonSidebarItems, loadAddonContributions } from "./addons.js";
// The theme applies before anything renders (M23: live, everywhere).
initThemeFromStorage();
/** Where the instance id lives: `/zim/servers/{id}/console`. */
function instanceIdFromPath() {
    const match = window.location.pathname.match(/^\/zim\/servers\/([^/]+)\/console\/?$/);
    return match !== null ? decodeURIComponent(match[1]) : null;
}
const nameHeading = document.getElementById("server-name");
const idPill = document.getElementById("server-id");
const dot = document.getElementById("status-dot");
const stateLabel = document.getElementById("state-label");
const consoleView = document.getElementById("console");
const meta = document.getElementById("console-meta");
const inputForm = document.getElementById("input-form");
const inputLine = document.getElementById("input-line");
const buttons = {
    start: document.getElementById("btn-start"),
    stop: document.getElementById("btn-stop"),
    restart: document.getElementById("btn-restart"),
    kill: document.getElementById("btn-kill"),
};
/** Which power operations make sense in a state (§10.3 gates). */
function powerAvailability(status) {
    return {
        start: status === "offline" || status === "suspended" || status === "crash_loop",
        stop: status === "running" || status === "starting",
        restart: status === "running",
        kill: status === "running" || status === "starting" || status === "stopping",
    };
}
function applyStatus(status) {
    if (dot !== null) {
        dot.setAttribute("status", status);
        dot.setAttribute("data-label", status);
    }
    if (stateLabel !== null) {
        stateLabel.textContent = `state: ${status}`;
    }
    const availability = powerAvailability(status);
    for (const [name, element] of Object.entries(buttons)) {
        if (element !== null) {
            element.disabled = !availability[name];
        }
    }
}
function appendEventLine(line) {
    if (consoleView !== null) {
        consoleView.appendLine(line);
    }
}
let stream = null;
let lastGeneration = 0;
async function boot() {
    const instanceId = instanceIdFromPath();
    if (instanceId === null) {
        appendEventLine(`zim: cannot parse a server id from ${window.location.pathname}`);
        return;
    }
    if (idPill !== null) {
        idPill.textContent = instanceId;
    }
    window.localStorage.setItem("zim.lastConsole", instanceId);
    // M24: the §4.5 sidebar contributions — enabled addons' items for
    // this instance render as a quicklinks nav under the console.
    // Best-effort: signed-out or unreachable engines render nothing.
    void loadAddonContributions().then(() => {
        renderAddonSurfaces(instanceId);
    });
    // Row (name) + observed state.
    try {
        const instance = await getInstance(instanceId);
        if (nameHeading !== null) {
            nameHeading.textContent = instance.name;
        }
        document.title = `ZIM — ${instance.name} console`;
    }
    catch (error) {
        reportError(error);
        return;
    }
    try {
        const snapshot = await getState(instanceId);
        applyStatus(snapshot.status);
    }
    catch (error) {
        reportError(error);
    }
    // Buffered console output (the replay baseline).
    try {
        const buffer = await consoleBuffer(instanceId);
        const lines = [];
        for (const event of buffer.events) {
            if (event.type === "instance.console_output" && typeof event.data.line === "string") {
                lines.push(event.data.line);
            }
        }
        if (consoleView !== null) {
            consoleView.replaceAll(lines);
        }
        lastGeneration = buffer.last_generation;
        if (meta !== null) {
            meta.textContent = `generation ${lastGeneration}`;
        }
    }
    catch (error) {
        reportError(error);
    }
    // The live stream.
    stream = new ZimStream(instanceId, {
        onEvent: (event) => {
            if (event.type === "instance.console_output" && typeof event.data.line === "string") {
                appendEventLine(event.data.line);
            }
            else if (event.type === "instance.state_change" && event.data.to !== undefined) {
                applyStatus(event.data.to);
            }
            lastGeneration = Math.max(lastGeneration, event.generation);
            if (meta !== null) {
                meta.textContent = `generation ${lastGeneration}`;
            }
        },
        onGap: () => {
            appendEventLine("zim: event gap detected — re-fetching state");
            void getState(instanceId)
                .then((snapshot) => {
                applyStatus(snapshot.status);
            })
                .catch(() => undefined);
        },
        onFatal: (reason) => {
            appendEventLine(`zim: live stream unavailable (${reason}); console shows buffered output only`);
        },
    });
}
function reportError(error) {
    if (error instanceof ApiError) {
        appendEventLine(`zim: ${error.code} — ${error.message}`);
        if (error.isAuth) {
            appendEventLine("zim: sign in from the new-tab page, then reload this console");
        }
    }
    else {
        appendEventLine(`zim: ${String(error)}`);
    }
}
/** Render the §4.5 addon sidebar contributions for this instance —
 * plain links into the addons' declared surfaces (view-only: the
 * targets are the addons' own pages). */
function renderAddonSurfaces(instanceId) {
    const items = addonSidebarItems(instanceId);
    const globals = addonSidebarItems(null);
    if (items.length === 0 && globals.length === 0) {
        return;
    }
    const section = document.createElement("section");
    section.className = "card addon-surfaces";
    section.setAttribute("aria-label", "Add-on surfaces");
    const heading = document.createElement("h2");
    heading.className = "card-title";
    heading.textContent = "Add-on surfaces";
    const nav = document.createElement("nav");
    nav.className = "quicklinks";
    nav.setAttribute("aria-label", "Add-on surfaces");
    const link = (item) => {
        const anchor = document.createElement("a");
        anchor.className = "quicklink";
        anchor.href = `/zim/${item.page.replace(/^zim:\/\//, "")}`;
        const icon = document.createElement("span");
        icon.className = "quicklink-icon";
        icon.setAttribute("aria-hidden", "true");
        icon.textContent = "+";
        const label = document.createElement("span");
        label.className = "quicklink-label";
        label.textContent = item.label;
        anchor.append(icon, label);
        return anchor;
    };
    for (const item of items) {
        nav.append(link(item));
    }
    for (const item of globals) {
        nav.append(link(item));
    }
    section.append(heading, nav);
    const main = document.querySelector("main.page-console");
    main?.append(section);
}
inputForm?.addEventListener("submit", (event) => {
    event.preventDefault();
    const instanceId = instanceIdFromPath();
    const line = inputLine?.value ?? "";
    if (instanceId === null || line === "") {
        return;
    }
    inputForm.reset();
    void consoleInput(instanceId, line)
        .then(() => {
        // The echoed line arrives on the stream (M7.1 behavior).
    })
        .catch((error) => {
        reportError(error);
    });
});
for (const [name, element] of Object.entries(buttons)) {
    element?.addEventListener("click", () => {
        const instanceId = instanceIdFromPath();
        if (instanceId === null) {
            return;
        }
        appendEventLine(`zim: requesting ${name}…`);
        void power(instanceId, name)
            .then(() => {
            appendEventLine(`zim: ${name} accepted`);
        })
            .catch((error) => {
            reportError(error);
        });
    });
}
window.addEventListener("beforeunload", () => {
    stream?.close();
});
void boot();
