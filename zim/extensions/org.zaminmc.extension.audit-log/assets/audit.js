/**
 * org.zaminmc.extension.audit-log — the audit viewer page logic.
 *
 * Loads the engine's audit trail over `GET /api/v1/audit` (admin-
 * gated) with cursor pagination, rendering newest-first. Non-admins
 * get an honest permission-denied state, not a fake empty list.
 * Every API call goes through the vendored SDK client (`Zim.api`),
 * which resolves this addon's registration first (`Zim.init`).
 */
(function () {
  "use strict";

  var ADDON_ID = "org.zaminmc.extension.audit-log";
  var PAGE_SIZE = 50;

  var stateEl = document.getElementById("state");
  var tableEl = document.querySelector(".audit-table");
  var rowsEl = document.getElementById("rows");
  var moreEl = document.getElementById("more");
  var reloadEl = document.getElementById("reload");
  var noteEl = document.getElementById("stream-note");

  var cursor = null;
  var hasMore = false;

  function setState(text, isError) {
    stateEl.textContent = text;
    stateEl.classList.toggle("error", Boolean(isError));
    stateEl.hidden = text === "";
  }

  function fmtTime(value) {
    if (typeof value !== "string") {
      return "";
    }
    var date = new Date(value);
    if (Number.isNaN(date.getTime())) {
      return value;
    }
    return date.toISOString().replace("T", " ").replace(".000Z", "Z");
  }

  function fmtActor(record) {
    var actor = record.actor;
    var label = typeof actor === "string" ? actor : String(actor || "unknown");
    if (typeof record.actor_id === "string" && record.actor_id !== "") {
      return label + " " + record.actor_id.slice(0, 8);
    }
    return label;
  }

  function fmtObject(record) {
    if (typeof record.instance_id === "string" && record.instance_id !== "") {
      var meta = record.metadata;
      var metaText = "";
      if (meta !== null && typeof meta === "object") {
        try {
          metaText = JSON.stringify(meta);
          if (metaText.length > 90) {
            metaText = metaText.slice(0, 87) + "…";
          }
        } catch (err) {
          metaText = "";
        }
      }
      return record.instance_id.slice(0, 8) + (metaText !== "" ? " " + metaText : "");
    }
    return "—";
  }

  function appendRows(records) {
    records.forEach(function (record) {
      var row = document.createElement("tr");
      var when = document.createElement("td");
      when.className = "when";
      when.textContent = fmtTime(record.timestamp);
      var actor = document.createElement("td");
      actor.textContent = fmtActor(record);
      var action = document.createElement("td");
      action.className = "action";
      action.textContent = record.action;
      var object = document.createElement("td");
      object.className = "mono";
      object.textContent = fmtObject(record);
      var request = document.createElement("td");
      request.className = "mono";
      request.textContent =
        record.metadata && typeof record.metadata.request_id === "string"
          ? record.metadata.request_id
          : "";
      row.appendChild(when);
      row.appendChild(actor);
      row.appendChild(action);
      row.appendChild(object);
      row.appendChild(request);
      rowsEl.appendChild(row);
    });
  }

  function load(reset) {
    if (reset) {
      cursor = null;
      hasMore = false;
      rowsEl.textContent = "";
      tableEl.hidden = true;
      moreEl.hidden = true;
      setState("Loading…", false);
    }
    var query = { limit: PAGE_SIZE };
    if (cursor !== null) {
      query.cursor = cursor;
    }
    Zim.api("/api/v1/audit", { query: query })
      .then(function (envelope) {
        var records = envelope.data || [];
        appendRows(records);
        var pagination = envelope.pagination || {};
        hasMore = Boolean(pagination.has_more);
        cursor = hasMore ? pagination.next_cursor : null;
        tableEl.hidden = rowsEl.children.length === 0;
        moreEl.hidden = !hasMore;
        if (rowsEl.children.length === 0 && reset) {
          setState("No audit records yet.", false);
        } else {
          setState("", false);
        }
        noteEl.hidden = true;
      })
      .catch(function (error) {
        if (error && error.isPermission) {
          setState(
            "Querying the audit log is admin-only. The signed-in user is not an " +
              "admin (missing grants: " +
              (error.missingPermissions && error.missingPermissions.length > 0
                ? error.missingPermissions.join(", ")
                : "admin") +
              ").",
            true,
          );
        } else if (error && error.isAuth) {
          setState("Sign in to query the audit log.", true);
        } else {
          setState(String(error && error.message ? error.message : error), true);
        }
      });
  }

  reloadEl.addEventListener("click", function () {
    load(true);
  });
  moreEl.addEventListener("click", function () {
    load(false);
  });

  // Boot: resolve this addon's registration (its granted permissions
  // — empty here, the audit endpoint follows the user's admin state —
  // and its enabled flag), then load the first page.
  Zim.init(ADDON_ID)
    .then(function (selfInfo) {
      if (selfInfo === null) {
        setState("This addon is not installed on the engine.", true);
        return;
      }
      if (!selfInfo.enabled) {
        setState("This addon is disabled.", true);
        return;
      }
      load(true);
    })
    .catch(function (error) {
      setState(String(error && error.message ? error.message : error), true);
    });
})();
