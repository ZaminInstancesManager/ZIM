/**
 * ZIM shell — boot page script.
 *
 * The shell owns this page (engine not involved). It listens for
 * engine-status events from the Rust side and mirrors the phase in
 * the status line; when the engine is ready the Rust side navigates
 * the window to `zim://newtab` (loopback-origin mapping, AD-23), so
 * this page never needs to navigate itself.
 *
 * Dependency-free; `window.__TAURI__` is provided by the shell's
 * `withGlobalTauri` config (absent only when the page is opened
 * outside the shell, e.g. a plain browser — the text still renders).
 */

(function () {
  "use strict";

  var statusEl = document.getElementById("status");
  if (!statusEl) {
    return;
  }

  var PHRASES = {
    waiting: "Starting the ZIM engine…",
    attaching: "Attaching to the ZIM engine…",
    spawning: "Starting the ZIM engine…",
    ready: "Engine ready — opening zim://newtab…",
    trouble: "The engine did not come up.",
  };

  function render(status) {
    var phase = status && status.phase;
    if (phase && PHRASES[phase]) {
      statusEl.textContent = PHRASES[phase];
    }
    if (phase === "trouble" && status && status.message) {
      statusEl.textContent = status.message;
    }
    document.body.dataset.phase = phase === "trouble" ? "down" : "boot";
  }

  var tauri = window.__TAURI__;
  if (tauri && tauri.event && tauri.event.listen) {
    tauri.event.listen("engine-status", function (event) {
      render(event.payload);
    });
    // Seed the line with the status at load time (the Rust side
    // emits on every transition; this catches the current one).
    if (tauri.core && tauri.core.invoke) {
      tauri.core
        .invoke("engine_status")
        .then(render)
        .catch(function () {
          /* the event stream will carry it */
        });
    }
  }
})();
