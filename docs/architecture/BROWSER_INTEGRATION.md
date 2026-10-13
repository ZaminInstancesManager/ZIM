# Browser Integration Architecture (Phase 6)

This document specifies how ZIM integrates with Chromium: the
`zim://` URL scheme, the per-tab server metadata, the WebUI
registration mechanism, the IPC client to the engine, the
session-restoration behavior, the split-view usage, and the
context-menu / keyboard-shortcut extensions.

It is canonical for the integration layer's design. The Chromium
audit (`docs/audits/CHROMIUM_ARCHITECTURE.md`) is canonical for what
Chromium provides; this document is canonical for what ZIM adds on
top.

## 1. Integration layer layout

The integration layer lives under `zim/integration/` (in the ZIM
repo) and is built as a Chromium-side shared library that the
browser process links against. The actual file paths in the Chromium
source tree (when imported) will be under `chrome/browser/zim/` and
will be added by a local patch under `chromium/PATCHES/`.

```
zim/integration/
├── webui_registration/        # registers the zim:// scheme + URL handlers
├── server_tab_feature/       # per-tab ZIM metadata; attaches via TabFeatures
├── tab_menu/                  # server-tab context-menu items
├── engine_ipc_client/        # HTTPS + WSS client to the engine
├── frontend_loader_bridge/   # asks the engine for the active frontend package
└── crash_keys/                # ZIM-specific Chromium crash keys
```

## 2. The `zim://` URL scheme

ZIM uses an internal URL scheme `zim://` for all ZIM-authored pages.
This is consistent with Chromium's WebUI infrastructure (the same
mechanism that powers `chrome://`, `chrome-untrusted://`, etc.).

### URL inventory

| URL                                  | What it loads                                       |
|--------------------------------------|-----------------------------------------------------|
| `zim://newtab`                       | ZIM new-tab page (replaces default NTP).            |
| `zim://servers`                      | Server list (overview of all instances).            |
| `zim://servers/{id}/console`        | Console for instance `{id}`.                        |
| `zim://servers/{id}/files`           | File browser for instance `{id}`.                   |
| `zim://servers/{id}/settings`        | Configuration for instance `{id}`.                  |
| `zim://servers/{id}/backups`         | Backup list for instance `{id}`.                    |
| `zim://servers/{id}/schedules`       | Schedule list for instance `{id}`.                  |
| `zim://servers/{id}/subusers`        | Subuser list for instance `{id}`.                   |
| `zim://servers/{id}/metrics`         | Resource metrics for instance `{id}`.               |
| `zim://extensions`                   | ZIM extension management.                           |
| `zim://settings`                     | ZIM settings (browser side).                        |
| `zim://settings/servers/{id}`        | Per-instance engine settings.                       |
| `zim://recovery`                     | Recovery surface (engine unreachable, etc.).        |

### Registration mechanism

`zim://` is registered as a WebUI scheme. The registration adds:

- A new scheme constant in `chrome/common/url_constants.h` (local
  Chromium patch `0001-zim-scheme.patch`).
- A `WebUIControllerFactory` extension that maps `zim://` hosts to
  ZIM's `WebUIController` subclasses.
- A content-security-policy that allows the `zim://` origin to
  fetch from the engine's loopback origin only (and to load assets
  from the active frontend package only).

The exact registration mechanism is INVESTIGATE (see
`docs/audits/COMPONENT_DISPOSITION.md` §5) — the file
`content/public/browser/web_ui.h` exists at the pinned revision but
its full API was not yet fetched. The integration's *behavior* is
specified here; the exact patch is identified for the implementation
phase.

### Security properties

- `zim://` pages run in their own renderer process (site-isolated
  from the open web). The page can talk to the engine via
  loopback HTTPS+WSS, with capability tokens; the page cannot talk
  to arbitrary origins.
- The page's CSP forbids loading scripts or styles from anywhere
  except the active frontend package's origin (served by the engine).
- Inline script is forbidden (matches Chromium's standard
  WebUI CSP).
- A `zim://` page can be loaded by the user directly (the omnibox
  accepts `zim://`), but cross-origin navigation from a regular web
  page is blocked by Chromium's standard scheme-security rules.

## 3. Per-tab server metadata

ZIM attaches server-tab metadata via Chromium's `TabFeatures`
mechanism (verified present at the pinned revision in
`chrome/browser/ui/tabs/tab_features.h`). ZIM's
`ServerTabFeature` is a `TabFeature` that:

- Holds the `InstanceId` the tab is currently viewing (if any).
- Subscribes to the instance's state stream when the tab is the
  active console tab.
- Updates the tab's visual indicator (green/yellow/red/blue/gray)
  when the instance's state changes.
- Unsubscribes when the tab navigates away from the `zim://servers/{id}/...`
  URL or when the tab is closed.

The visual indicator is rendered by extending `TabStyle`
(`chrome/browser/ui/tabs/tab_style.h` at the pinned revision). ZIM
contributes a `ServerTabStyle` decorator that adds the colored
dot/badge without modifying the existing tab rendering.

### Status model → visual mapping

Per the prompt's status model:

| Engine state                       | Visual color | Notes |
|------------------------------------|--------------|-------|
| Running                            | Green        | Solid green dot. |
| Starting                           | Yellow       | Pulsing yellow. |
| Stopping                           | Yellow       | Pulsing yellow (same as starting; transient). |
| Crashed (single crash, not loop)   | Red          | Solid red; tooltip shows "crashed at {time}". |
| CrashLoop                           | Red          | Red with caution glyph; tooltip shows "crash loop — N crashes in T minutes". |
| Offline (stopped, idle)            | Blue         | Solid blue. |
| Installing                          | Yellow       | With progress overlay (0-100%). |
| Suspended                          | Gray         | Solid gray. |
| RestoringBackup                    | Yellow       | With progress overlay. |
| Transferring                       | Yellow       | With progress overlay + transfer-source/dest tooltip. |
| Unknown (runtime unreachable / node disconnected) | Gray | Solid gray; tooltip shows "engine cannot reach runtime". |

The four primary statuses (green/yellow/red/blue) match the prompt's
required model. Other states are shown without misrepresenting the
four primaries — installing uses yellow with a progress overlay (not
solid yellow); crash-loop uses red with a glyph (not solid red).

## 4. Server-tab context menu

ZIM adds server-tab context-menu items via Chromium's
`TabMenuModel` extension path (`chrome/browser/ui/tabs/tab_menu_model.{h,cc}`
at the pinned revision). ZIM's additions:

- "Start server" — only if observed state is Offline and not in
  crash-loop.
- "Stop server" — only if observed state is Running or Starting.
- "Restart server" — only if observed state is Running.
- "Open in split view" — opens a second pane with the same server's
  console (or another surface); uses Chromium's existing split-view
  API (no ZIM patch needed; the menu item simply invokes the existing
  `split_tab_menu_model` actions).
- "Copy server URL" — copies the `zim://servers/{id}/console` URL.
- "Duplicate" — duplicates the tab (Chromium-native; ZIM
  doesn't override).

Each item's visibility is gated by the engine's permission check (a
`control.start` permission is required to show "Start server", etc.).

## 5. IPC client to the engine

The browser-side IPC client (`zim/integration/engine_ipc_client/`)
is a C++ component that talks to the engine over HTTPS+WSS on
loopback. The client:

- Authenticates to the engine with a per-tab capability token
  (issued by the engine when the tab navigates to a `zim://` page
  that requires permissions; the token is scoped to that tab's
  permission set).
- Manages the WebSocket connection's lifecycle: connect on tab
  focus, disconnect on tab close (with a configurable keep-alive
  timeout so that briefly switching tabs doesn't drop the WS).
- Handles reconnect with replay-from-checkpoint (sends the last-seen
  generation on reconnect).
- Surfaces structured errors to the page (the page renders them
  according to the active frontend's UX).

The client is implemented in C++ (not Rust) because it lives inside
the Chromium browser process; it cannot depend on the engine's Rust
crates. The client uses Chromium's standard `network::SimpleURLLoader`
for HTTPS and Chromium's WebSocket implementation for WSS — both
preserved from upstream.

### Loopback-only

The engine listens on `127.0.0.1` (or a Unix-domain socket on
Linux/macOS). The browser's IPC client connects only to the loopback
address. This is enforced by the engine (which binds to loopback
only) and by the IPC client (which refuses to connect elsewhere).

For remote-node coordination, the engine (not the browser) opens a
separate, authenticated, versioned connection to the remote node;
the browser is never directly involved in remote-node communication.

## 6. Session restoration behavior

Per `ZIM_ARCHITECTURE.md` §4 invariants 3-5, session restoration must
restore *views*, not issue server lifecycle commands. The
integration's role:

- A restored tab navigates to its `zim://` URL.
- The loaded page queries the engine for the instance's current
  state and subscribes.
- No lifecycle API is called as a side effect of session restore.

This is enforced by the integration layer:

- The IPC client does *not* automatically call lifecycle APIs on
  page load. The page must explicitly request a lifecycle operation
  in response to a user action.
- The engine does *not* infer server lifecycle from tab presence;
  the engine has no concept of tabs.

## 7. Split-view usage

Chromium 157's split-view API is verified first-class (see
`docs/audits/CHROMIUM_ARCHITECTURE.md` §6.4). ZIM uses it, not a
parallel implementation:

- "Open in split view" (per §4) invokes the existing
  `split_tab_menu_model` actions to create a split with the current
  tab and a new tab.
- The new tab navigates to a `zim://` URL (the user's choice or the
  same instance's other surface, e.g. console + files).
- Each pane has its own `WebContents` and its own
  `ServerTabFeature`; the engine treats them as independent
  subscribers.
- Closing one pane unsubscribes that pane; the server keeps
  running.
- Resizing, layout switching, pane focus, keyboard navigation, and
  swap are all upstream Chromium behavior; ZIM does not modify them.

The split-view's session persistence is also upstream: a window with
a split is restored with the split.

## 8. New-tab page (NTP) replacement

The default Chromium NTP (`chrome://newtab`) is replaced by ZIM's
`zim://newtab`. The replacement is via WebUI registration (not by
patching the upstream NTP source):

- ZIM's `WebUIControllerFactory` returns ZIM's controller for the
  `zim://newtab` host.
- The default-NTP controller is preserved (the user can navigate
  to `chrome://newtab` to see the upstream NTP if desired).
- The ZIM NTP is part of the default frontend package; replacing
  the frontend package replaces the NTP.

## 9. Crash keys

ZIM registers Chromium crash keys for:

- The active frontend package's version and manifest hash.
- The engine's version and uptime at the time of the browser crash.
- Whether any tab was a server tab at the time of the crash.

These are diagnostic and do not leak server state to crash reports
(per `docs/security/THREAT_MODEL.md`).

## 10. Profile separation

Chromium profiles (per-user browser profiles, e.g. "Default",
"Work") have separate session services, separate bookmarks, separate
history. ZIM treats each browser profile as a separate user context:

- A tab in profile A and a tab in profile B for the same instance
  are independent subscribers.
- The engine's permission model is per-tab (the per-tab capability
  token is scoped to the active profile's user identity, resolved by
  the engine when the tab connects).

## 11. What the integration does NOT do

- It does not modify the omnibox.
- It does not modify the address bar.
- It does not modify bookmarks or history.
- It does not modify the tab strip's rendering of pinned tabs,
  tab groups, or recently closed (these are all upstream and
  preserved).
- It does not modify Chromium's keyboard shortcut table for
  browser shortcuts; ZIM-specific shortcuts are registered through
  `BrowserWindowFeatures`, scoped to the active ZIM surface.
- It does not weaken the renderer sandbox.
- It does not run the engine in-process.
- It does not allow web content to access the engine directly.

## 12. Local Chromium patches required

The integration requires the following local Chromium patches (to be
applied under `chromium/PATCHES/` when the source tree is imported):

| Patch number | Purpose                                                              | Evidence |
|--------------|----------------------------------------------------------------------|----------|
| 0001         | Register `zim://` scheme + URL constants.                          | `chrome/common/url_constants.h` enumerated at the pinned revision. |
| 0002         | Register `WebUIControllerFactory` for `zim://` hosts.              | `chrome/browser/ui/webui/` infrastructure present at the pinned revision. |
| 0003         | Add `ServerTabFeature` to the `TabFeatures` graph.                | `chrome/browser/ui/tabs/tab_features.h` present at the pinned revision (full API is INVESTIGATE). |
| 0004         | Extend `TabStyle` with `ServerTabStyle` decorator.                | `chrome/browser/ui/tabs/tab_style.{h,cc}` present at the pinned revision. |
| 0005         | Add ZIM items to `TabMenuModel`.                                   | `chrome/browser/ui/tabs/tab_menu_model.{h,cc}` present at the pinned revision. |
| 0006         | Register ZIM crash keys.                                            | Chromium's crash-key registration mechanism (`components/crash/`). |
| 0007 (opt)   | Disable specific Chromium features not wanted in ZIM (e.g. some bundled applications). | Candidates identified during implementation; documented in `docs/audits/REMOVALS.md` candidate R5. |

Each patch is small, additive, and self-documenting; no patch
modifies upstream behavior except where the change is explicitly
documented in this architecture and the patch file.

## 13. Open integration questions

1. The exact `WebUIControllerFactory` registration mechanism for a
   custom scheme is INVESTIGATE; the implementation phase will
   confirm via direct source reading.
2. The exact `TabFeatures` attachment API is INVESTIGATE.
3. Whether ZIM should use a separate scheme (`zim://`) or an
   existing scheme (`chrome://zim/`) is recorded as an open decision
   in `docs/architecture/DECISIONS.md` (preliminary: separate scheme
   for clearer privilege separation, but a sub-path under
   `chrome://` would require fewer patches).
