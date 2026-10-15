# Desktop Frontend Integration Architecture

> **CORRECTION (2026-10-15, AD-20):** This document was originally
> titled "Browser Integration Architecture" and specified a
> Chromium-fork integration (zim:// WebUI patches, TabFeatures C++
> hooks, Chromium patch queue). That model is **superseded**: ZIM is
> a desktop application with a Chrome-**inspired** interaction model,
> not a browser. Chrome's UX — tab strip behavior, animations, split
> views, new-tab experience, address bar — is the design reference we
> replicate in our own frontend, not a dependency on Chromium source.
> See `docs/architecture/DECISIONS.md` AD-20/AD-21/AD-22. The URL
> inventory, per-tab status model, engine API contract, and
> session-restoration invariants below are **preserved** from the
> original design; only the hosting mechanism changed (system WebView
> desktop shell instead of a patched Chromium).

This document specifies how the ZIM desktop frontend is structured:
the shell architecture, the `zim://` internal navigation namespace,
the per-tab server metadata and status model, the engine's IPC
contract from the frontend's perspective, session restoration, split
views, and the Chrome-UX reference inventory that the frontend
replicates.

It is canonical for the frontend's design. The Pterodactyl audit
(`docs/audits/PTERODACTYL_ARCHITECTURE.md`) is canonical for the
management capabilities; `docs/architecture/ZIM_ARCHITECTURE.md` is
canonical for the engine and system invariants.

## 1. What the frontend is

ZIM's frontend is a **desktop management application** — not a
general-purpose browser. It provides:

- A top tab strip with one tab per managed server or workload view.
- A new-tab page for discovering, creating, importing, and opening
  servers.
- A Chrome-inspired address/navigation bar adapted to ZIM's internal
  navigation (`zim://` URLs).
- Split views, multiple windows, tab organization, and session
  restoration.
- A dashboard and dedicated server views: console, files,
  configuration, startup settings, ports, databases, backups,
  monitoring, schedules, subusers, and extensions.
- Themes, customization, and a documented extension/addon system.

The frontend is written in HTML/CSS/TypeScript (the existing
dependency-free Web-Components package under `zim/frontend/`), is
served by the engine over loopback HTTPS, and is hosted by a thin
desktop shell (AD-21). It never implements ordinary website browsing,
browser history, browser bookmarks, a Chromium extension runtime, or
browser security infrastructure.

**The frontend is replaceable** via the frontend-package manifest
(`docs/api/FRONTEND_EXTENSION_SPEC.md`): the engine loads the active
package; a broken frontend cannot lock the user out of the engine.

## 2. Shell architecture (AD-21)

```
┌─────────────────────────────────────────────────────┐
│ Desktop shell (zim/shell/, Tauri 2)                 │
│  window management · OS integration · tray           │
│  ┌───────────────────────────────────────────────┐  │
│  │ System WebView (WebView2/WKWebView/WebKitGTK) │  │
│  │  shell UI: tab strip, address bar, split view │  │
│  │  content: engine-served frontend pages        │  │
│  └───────────────────────────────────────────────┘  │
└──────────────┬──────────────────────────────────────┘
               │ loopback HTTPS + WSS only
┌──────────────▼──────────────────────────────────────┐
│ Rust engine (zim/engine/) — authoritative state      │
│  serves the active frontend package + the API       │
└─────────────────────────────────────────────────────┘
```

Key properties:

- **The shell uses the OS webview** — Edge WebView2 on Windows,
  WKWebView on macOS, WebKitGTK on Linux. ZIM does not build, patch,
  import, or ship Chromium source. Using the platform webview is how
  ordinary desktop apps render HTML; it carries none of the
  browser-engine maintenance burden.
- **The engine is the authority and survives the shell.** Closing all
  shell windows, crashing the shell, or killing it does not stop,
  start, or otherwise touch any server. The engine is an independent
  OS process with its own supervision and persistence
  (`ZIM_ARCHITECTURE.md` §4 invariants).
- **The shell is thin.** All management UI logic lives in the
  engine-served frontend package; the shell owns only windowing, OS
  integration, and the app chrome (tab strip, address bar, split
  containers). This keeps the "replaceable frontend" property intact
  and keeps the shell a bounded, testable component.
- **All engine traffic is loopback HTTPS+WSS** with per-tab
  capability-token auth (`ZIM_API_SPEC.md`), exactly as before. The
  shell never proxies engine traffic and never talks to remote nodes;
  remote-node coordination is engine-internal.

## 3. The `zim://` navigation namespace

`zim://` is ZIM's internal navigation namespace — the app's URL
vocabulary for its own pages. It is served by the engine's frontend
loader (the shell maps the namespace onto the engine's loopback
origin / a shell custom protocol; the mapping strategy is an M21
spike decision).

### URL inventory

| URL                                  | What it shows                                        |
|--------------------------------------|------------------------------------------------------|
| `zim://newtab`                       | New-tab page (server discovery / create / import).   |
| `zim://servers`                      | Server list (overview of all instances).             |
| `zim://servers/{id}/console`        | Console for instance `{id}`.                          |
| `zim://servers/{id}/files`           | File browser for instance `{id}`.                     |
| `zim://servers/{id}/settings`        | Configuration for instance `{id}`.                    |
| `zim://servers/{id}/backups`         | Backup list for instance `{id}`.                      |
| `zim://servers/{id}/schedules`       | Schedule list for instance `{id}`.                    |
| `zim://servers/{id}/subusers`        | Subuser list for instance `{id}`.                     |
| `zim://servers/{id}/metrics`         | Resource metrics for instance `{id}`.                 |
| `zim://workloads/{type}/...`        | Workload-type surfaces (AD-19 canonical form).        |
| `zim://extensions`                   | ZIM extension management.                             |
| `zim://settings`                     | ZIM settings (application side).                      |
| `zim://settings/servers/{id}`        | Per-instance engine settings.                         |
| `zim://recovery`                     | Recovery surface (engine unreachable, etc.).          |

These URLs are **stable identifiers**: session restoration,
bookmarks-style quick access, split-view pairings, and extension
links all address pages by `zim://` URL, never by tab index.

## 4. Per-tab server metadata and the status model

Each tab that displays a `zim://servers/{id}/...` view holds
per-tab server metadata: the `InstanceId` being viewed, its
subscription to the instance's event stream, and the presentation
state derived from the engine's reported status. The tab's indicator
updates in real time from `instance.state_change` events over the
stream; it unsubscribes on navigation away or tab close. Closing the
tab only unsubscribes — it never issues lifecycle commands
(`ZIM_ARCHITECTURE.md` §4 invariants 3-5).

The browser-side status model mirrors the engine's canonical
`domain::InstanceStatus` exactly (ten variants, snake_case wire
names); the frontend never invents its own states.

### Status model → visual mapping

| Engine state                       | Visual color | Notes |
|------------------------------------|--------------|-------|
| Running                            | Green        | Solid green dot. |
| Starting                           | Yellow       | Pulsing yellow. |
| Stopping                           | Yellow       | Pulsing yellow (same as starting; transient). |
| CrashLoop                           | Red          | Red with caution glyph; tooltip shows "crash loop — N crashes in T minutes". |
| Offline (single crash, not loop)   | Blue         | Solid blue; tooltip shows "crashed at {time}" via crash metadata. |
| Installing                          | Yellow       | With progress overlay (0-100%). |
| Suspended                           | Gray         | Solid gray. |
| RestoringBackup                    | Yellow       | With progress overlay. |
| Transferring                       | Yellow       | With progress overlay + transfer-source/dest tooltip. |
| Unknown (runtime unreachable / node disconnected) | Gray | Solid gray; tooltip shows "engine cannot reach runtime". |

The four primary statuses (green/yellow/red/blue) match the required
model. Extension states reuse the primaries with overlays rather
than inventing colors: installing uses yellow + progress overlay
(not solid yellow); crash-loop uses red + glyph (not solid red).

## 5. Server-tab context menu

The tab context menu on a server tab offers (visibility gated by the
engine's permission check — `control.start` etc.):

- **Open console / files / settings / backups / metrics** —
  navigates or opens the respective `zim://servers/{id}/...` view.
- **Start server** — only if observed state is Offline and not in
  crash-loop. Requires `control.start`.
- **Stop server** — only if observed state is Running or Starting.
  Requires `control.stop`.
- **Restart server** — only if observed state is Running. Requires
  `control.restart`.
- **Open in split view** — opens a second pane with the same
  server's console or another surface.
- **Copy server URL** — copies the `zim://servers/{id}/console` URL.

"Duplicate tab" is shell-native behavior and needs no ZIM-specific
handling.

## 6. Engine IPC from the frontend

The frontend talks to the engine over loopback HTTPS+WSS exactly as
the web frontend always has (`ZIM_API_SPEC.md`):

- HTTPS requests authenticate with per-tab capability tokens issued
  by the engine for `zim://` pages requiring permissions
  (`Authorization: Bearer {token}`), scoped to that tab's permission
  set.
- The streaming endpoint `wss://127.0.0.1:{port}/api/v1/stream`
  carries console output, state changes, resource usage, and job
  progress events, with replay-from-checkpoint
  (`?since_generation={n}`) on reconnect and drop-oldest
  backpressure.
- The engine binds loopback only; the frontend connects loopback
  only. Remote-node coordination is engine-internal; the frontend is
  never directly involved.

The frontend renders structured engine errors according to the
active frontend package's UX; it never auto-calls lifecycle APIs on
page load (session restoration restores views, not commands).

## 7. Session restoration

Session restoration restores **views**, never server lifecycle:

- The shell persists its window/tab/split layout as `zim://` URLs
  (stable identifiers, §3).
- On restore, each tab navigates to its URL; the loaded page queries
  the engine for current state and subscribes.
- No lifecycle API is called as a side effect of restoration. The
  engine has no concept of tabs; it cannot infer lifecycle from tab
  presence.

## 8. Split view

Split views are a shell-level construct: two panes, each hosting an
independent engine-served view (e.g. console + files of the same
instance, or two different servers). Each pane has its own stream
subscription and per-tab metadata; closing one pane only
unsubscribes that pane. Layout switching, resizing, and focus order
follow the Chrome split-view interaction patterns in §12.

## 9. New-tab page

The new-tab page (`zim://newtab`) is the surface for discovering,
creating, importing, and opening servers. It is part of the active
frontend package; replacing the frontend package replaces the NTP.
Its layout follows the Chrome NTP structure adapted to ZIM: a search
/filter field, frequently- and recently-opened servers, create/import
actions, and workload-type browsing.

## 10. Diagnostics

There are no Chromium crash keys. Shell diagnostics (shell version,
frontend package id/version/manifest hash, engine version, whether
any tab was a server tab) are reported to the **engine's** audit/diagnostics
surface, where they ride the same retention rules as other engine
records (`THREAT_MODEL.md` — no server state leaks beyond what the
engine already records).

## 11. What the frontend does NOT do

- Ordinary website browsing: navigation outside `zim://` (and the
  engine-served assets) is not offered.
- Browser history / bookmarks infrastructure: ZIM's quick-access and
  recent-servers features are engine-backed app state, not a browser
  history database.
- Chromium extension runtime: ZIM extensions are the engine's own
  addon system (`FRONTEND_EXTENSION_SPEC.md`), not Chrome extensions.
- Browser security infrastructure (sandboxing renderers, site
  isolation, etc.): the attack surface is the engine's loopback API +
  the OS webview runtime, addressed in `THREAT_MODEL.md`.
- Authority over workload state: the frontend renders engine state;
  it never derives or owns it.

## 12. Chrome-UX reference inventory

The interaction model is deliberately copied from Chrome's UX so the
app feels familiar. This is a **design reference** — we replicate the
behavior and feel in our own TypeScript/CSS, we do not vendor
Chrome's code. The reference inventory (behaviors to replicate,
with their Chrome counterparts):

| ZIM surface            | Chrome UX being replicated                                  |
|------------------------|--------------------------------------------------------------|
| Tab strip              | Tab shapes, hover states, drag-reorder, drag-out-to-window, close-target expansion, new-tab button placement, tab strip overflow. |
| Tab state indicators   | Chrome's favicon/throbber slot repurposed for the workload status dot (§4 mapping). |
| Animations             | Chrome's tab open/close curves, tab-drag lift, split-view divider physics, omnibox focus ring. |
| Address bar            | Omnibox layout, focus behavior, in-place security/origin indication (engine identity instead of TLS site identity), suggestion dropdown behavior. |
| Split view             | Chrome 126+ split-tab interaction: divider drag, pane focus, resize snapping, split-all/restore. |
| New-tab page           | NTP layout grammar: centered search, tile grid, customization entry points. |
| Session restore        | Chrome's "restore session" UX, applied to zim:// tab sets. |
| Context menus          | Chrome's tab context menu structure and item ordering, with ZIM items (§5). |
| Themes                 | Chrome's theme model (color pip + preset rows) adapted to ZIM theme packages. |

Each replicated behavior gets a UX-parity checklist entry in the
M22/M23 milestones (`IMPLEMENTATION_ROADMAP.md`) and a
front-end test where feasible (tab reorder logic, indicator mapping,
session persistence). Animation values (durations, curves) are tuned
against Chrome's publicly documented motion specs, not copied from
source.

## 13. Open integration questions

1. Shell content hosting strategy: single webview with iframe-based
   panes vs. multiple native webviews per tab/pane (M21 spike;
   trade-off is memory vs. per-pane isolation and fidelity).
2. `zim://` resolution: shell custom protocol (Tauri `register_asynchronous_protocol`)
   vs. engine loopback origin with URL rewriting (M21 spike).
3. Webview divergence testing matrix: which WebKitGTK/WebView2/
   WKWebView versions are the floor (M22; the dependency-free
   frontend keeps the surface small).
4. Shell auto-update channel mechanics (deferred to M25 packaging).
