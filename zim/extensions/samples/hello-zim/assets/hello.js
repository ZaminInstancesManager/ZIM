/**
 * org.example.zim-addon.hello-zim — the sample addon's page logic.
 *
 * A complete tour of the SDK in the smallest honest surface:
 *
 * 1. `Zim.init` resolves the addon's own registration.
 * 2. The {id} path parameter is recovered from the page URL.
 * 3. `Zim.api` reads the instance (an unauthenticated-ish read —
 *    the engine's authz answers, as always).
 * 4. `new Zim.ZimStream` subscribes to the instance's §11 stream
 *    (the manifest's `websocket.connect` permission + event
 *    subscription made real) and counts `instance.state_change`.
 * 5. The query string shows context-menu targeting
 *    (`?from=context-menu`).
 */
(function () {
  "use strict";

  var ADDON_ID = "org.example.zim-addon.hello-zim";

  var instanceEl = document.getElementById("instance");
  var nameEl = document.getElementById("instance-name");
  var stateEl = document.getElementById("state");
  var viaEl = document.getElementById("via");
  var eventsEl = document.getElementById("events");
  var noteEl = document.getElementById("note");

  // The surface is zim://servers/{id}/hello, served at
  // /zim/servers/{id}/hello — the instance id is the second path
  // segment.
  var segments = window.location.pathname.replace(/^\/+/, "").split("/");
  var instanceId = segments.length >= 3 && segments[0] === "zim" ? segments[2] : null;
  instanceEl.textContent = instanceId === null ? "(no instance in the URL)" : instanceId;

  var via = new URLSearchParams(window.location.search).get("from");
  if (via !== null) {
    viaEl.textContent = via;
  }

  var eventsSeen = 0;

  function note(text) {
    noteEl.textContent = text;
  }

  Zim.init(ADDON_ID)
    .then(function (selfInfo) {
      if (selfInfo === null) {
        note("This addon is not installed on the engine.");
        return null;
      }
      if (instanceId === null) {
        note("This page belongs to a per-instance surface.");
        return null;
      }
      return Zim.api("/api/v1/instances/" + encodeURIComponent(instanceId))
        .then(function (envelope) {
          var instance = envelope.instance || envelope;
          nameEl.textContent = typeof instance.name === "string" ? instance.name : "(unnamed)";
          stateEl.textContent =
            typeof instance.state === "string"
              ? instance.state
              : typeof instance.status === "string"
                ? instance.status
                : "(state follows the §4 indicator once the stream connects)";
          note("The addon is installed and enabled; its granted permissions are: " +
            (selfInfo.permissions.granted.length > 0
              ? selfInfo.permissions.granted.join(", ")
              : "(none declared)") + ".");
        })
        .catch(function (error) {
          if (error && error.isAuth) {
            note("Sign in to read this instance.");
          } else if (error && error.isPermission) {
            note("No access to this instance: " + error.message);
          } else {
            note(String(error && error.message ? error.message : error));
          }
        });
    })
    .catch(function (error) {
      note(String(error && error.message ? error.message : error));
    });

  // The manifest's event subscription, made real: one §11 stream,
  // counting state changes (and honestly surfacing stream gaps).
  if (instanceId !== null && typeof WebSocket !== "undefined") {
    var stream = new Zim.ZimStream(instanceId, {
      onEvent: function (event) {
        if (event.type === "instance.state_change") {
          eventsSeen += 1;
          eventsEl.textContent = String(eventsSeen);
          var data = event.data || {};
          stateEl.textContent =
            typeof data.state === "string" ? data.state : stateEl.textContent;
        }
      },
      onStatus: function (status) {
        if (status === "live") {
          stateEl.textContent = stateEl.textContent === "…" ? "live" : stateEl.textContent;
        }
      },
      onGap: function () {
        note("A stream gap arrived — the page re-fetches over HTTP rather than trusting a partial replay.");
      },
      onFatal: function (reason) {
        note("The event stream could not be established: " + reason);
      },
    });
    window.addEventListener("beforeunload", function () {
      stream.close();
    });
  }
})();
