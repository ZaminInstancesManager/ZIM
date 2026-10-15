/**
 * `zim://servers/{id}/console` — the server console.
 *
 * The §10.x data surfaces (row, observed state, buffered console)
 * followed by the live §11 stream: state changes re-render the power
 * controls, console output appends to the log, and input is sent via
 * the HTTP endpoint (the v1 WS contract has no client→server frames).
 */

import { ApiError, consoleBuffer, consoleInput, getInstance, getState, power } from "./api";
import { ZimStream } from "./stream";
import "./components";
import { initThemeFromStorage } from "./theme";

// The theme applies before anything renders (M23: live, everywhere).
initThemeFromStorage();

/** Where the instance id lives: `/zim/servers/{id}/console`. */
function instanceIdFromPath(): string | null {
  const match = window.location.pathname.match(/^\/zim\/servers\/([^/]+)\/console\/?$/);
  return match !== null ? decodeURIComponent(match[1]) : null;
}

const nameHeading = document.getElementById("server-name");
const idPill = document.getElementById("server-id");
const dot = document.getElementById("status-dot");
const stateLabel = document.getElementById("state-label");
const consoleView = document.getElementById("console");
const meta = document.getElementById("console-meta");
const inputForm = document.getElementById("input-form") as HTMLFormElement | null;
const inputLine = document.getElementById("input-line") as HTMLInputElement | null;

const buttons = {
  start: document.getElementById("btn-start") as HTMLButtonElement | null,
  stop: document.getElementById("btn-stop") as HTMLButtonElement | null,
  restart: document.getElementById("btn-restart") as HTMLButtonElement | null,
  kill: document.getElementById("btn-kill") as HTMLButtonElement | null,
};

/** Which power operations make sense in a state (§10.3 gates). */
function powerAvailability(status: string): Record<string, boolean> {
  return {
    start: status === "offline" || status === "suspended" || status === "crash_loop",
    stop: status === "running" || status === "starting",
    restart: status === "running",
    kill: status === "running" || status === "starting" || status === "stopping",
  };
}

function applyStatus(status: string): void {
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

function appendEventLine(line: string): void {
  if (consoleView !== null) {
    (consoleView as import("./components").ZimConsole).appendLine(line);
  }
}

let stream: ZimStream | null = null;
let lastGeneration = 0;

async function boot(): Promise<void> {
  const instanceId = instanceIdFromPath();
  if (instanceId === null) {
    appendEventLine(`zim: cannot parse a server id from ${window.location.pathname}`);
    return;
  }
  if (idPill !== null) {
    idPill.textContent = instanceId;
  }
  window.localStorage.setItem("zim.lastConsole", instanceId);

  // Row (name) + observed state.
  try {
    const instance = await getInstance(instanceId);
    if (nameHeading !== null) {
      nameHeading.textContent = instance.name;
    }
    document.title = `ZIM — ${instance.name} console`;
  } catch (error) {
    reportError(error);
    return;
  }
  try {
    const snapshot = await getState(instanceId);
    applyStatus(snapshot.status);
  } catch (error) {
    reportError(error);
  }

  // Buffered console output (the replay baseline).
  try {
    const buffer = await consoleBuffer(instanceId);
    const lines: string[] = [];
    for (const event of buffer.events) {
      if (event.type === "instance.console_output" && typeof event.data.line === "string") {
        lines.push(event.data.line);
      }
    }
    if (consoleView !== null) {
      (consoleView as import("./components").ZimConsole).replaceAll(lines);
    }
    lastGeneration = buffer.last_generation;
    if (meta !== null) {
      meta.textContent = `generation ${lastGeneration}`;
    }
  } catch (error) {
    reportError(error);
  }

  // The live stream.
  stream = new ZimStream(instanceId, {
    onEvent: (event) => {
      if (event.type === "instance.console_output" && typeof event.data.line === "string") {
        appendEventLine(event.data.line);
      } else if (event.type === "instance.state_change" && event.data.to !== undefined) {
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

function reportError(error: unknown): void {
  if (error instanceof ApiError) {
    appendEventLine(`zim: ${error.code} — ${error.message}`);
    if (error.isAuth) {
      appendEventLine("zim: sign in from the new-tab page, then reload this console");
    }
  } else {
    appendEventLine(`zim: ${String(error)}`);
  }
}

inputForm?.addEventListener("submit", (event: Event) => {
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
    .catch((error: unknown) => {
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
      .catch((error: unknown) => {
        reportError(error);
      });
  });
}

window.addEventListener("beforeunload", () => {
  stream?.close();
});

void boot();
