/**
 * ZIM shell — recovery page script.
 *
 * Shown when the engine is unreachable or the loopback certificate
 * could not be trusted: the detail line carries the diagnostic and
 * the Retry button re-runs the attach sequence in the Rust side
 * (engine_status / engine_retry commands). Navigating away happens
 * only from Rust when the engine answers.
 */

(function () {
  "use strict";

  var detailEl = document.getElementById("detail");
  var retryBtn = document.getElementById("retry");

  var tauri = window.__TAURI__;

  function render(status) {
    if (detailEl && status && status.message) {
      detailEl.textContent = status.message;
    } else if (detailEl && status && status.phase) {
      detailEl.textContent =
        "Engine status: " + status.phase + ".";
    }
  }

  if (retryBtn) {
    retryBtn.addEventListener("click", function () {
      retryBtn.disabled = true;
      retryBtn.textContent = "Retrying…";
      if (tauri && tauri.core && tauri.core.invoke) {
        tauri.core
          .invoke("engine_retry")
          .catch(function () {
            /* status events still arrive */
          })
          .finally(function () {
            retryBtn.disabled = false;
            retryBtn.textContent = "Retry";
          });
      }
    });
  }

  if (tauri && tauri.event && tauri.event.listen) {
    tauri.event.listen("engine-status", function (event) {
      render(event.payload);
    });
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
