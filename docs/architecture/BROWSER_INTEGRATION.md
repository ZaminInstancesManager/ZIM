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
loader; the shell maps the namespace onto the engine's loopback
origin at navigation time (AD-23; see §13's resolution notes).

### URL inventory

| URL                                  | What it shows                                        |
|--------------------------------------|------------------------------------------------------|
| `zim://app`                          | The application chrome (M22): tab strip + omnibox, hosting the per-tab panes. |
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

M22 note on `zim://app` and the per-instance surfaces: the chrome
is a declared frontend surface like any other — it renders in any
browser, not only the desktop shell's webview. Its tab panes are
same-origin iframes onto the other surfaces (AD-24), so the chrome,
the panes, and direct page visits all share one URL space and one
set of view-only semantics. The six per-instance rows (files,
settings, backups, schedules, subusers, metrics) are declared and
served by the default package as of M22 — with honest stub bodies
(the metrics surface renders real recent samples); their full views
ship with the milestones that own them.

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

M22 implementation notes: the mapping is realized in the
`zim-status-dot` component — pulsing for the transient yellow
states, a conic progress ring driven by `instance.install_progress`
events, the crash-loop glyph, and a small tab-strip variant in the
favicon slot. The engine's `visual_color` mirror was aligned with
this table in M22: `Suspended` is gray (the engine mirror
previously said blue; this table is the visual authority, and
Offline alone keeps blue).

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

**Implementation (M23):** a split is tab-level state — `{path,
ratio}` on the tab, one split per tab (Chrome's split tab is two
panes, not a tree). Each pane is its own iframe with its own §11
stream; the divider drags with snap points at the halves and
thirds (3% tolerance, visibly marked), double-click re-centers,
arrow keys nudge, and the divider's close affordance (or the tab
menu's "Close split pane") closes only the secondary pane — one
`stream.close()`, the primary pane and the server untouched. The
omnibox navigates the focused pane. The pairing serializes into
session v2 (`tabs.ts`) with a clamped ratio; a torn-out split tab
carries both paths. Not shipped: split-all and cross-window
drag-merge (polish; the menu-driven merge is the M23 path).

## 9. New-tab page

The new-tab page (`zim://newtab`) is the surface for discovering,
creating, importing, and opening servers. It is part of the active
frontend package; replacing the frontend package replaces the NTP.
Its layout follows the Chrome NTP structure adapted to ZIM: a search
/filter field, frequently- and recently-opened servers, create/import
actions, and workload-type browsing.

**Implementation (M23):** the centered search filters the tile grids
live (name or id). Recents are engine-backed — `GET /me/recents`
(user-scoped rows, capped at the newest 32, ranked by last-open)
and the chrome records pane adoptions fire-and-forget (`POST
/me/recents`); frequent tiles badge their open counts. Create is the
engine's admin-gated `POST /instances` (node picker included; a 403
on node listing says so instead of offering a doomed button);
import parses a pasted ZIM server-definition JSON through the same
field validation (pure, unit-tested) and points panel migrations at
the `zim-migrate-pterodactyl` CLI — a paste box does not migrate a
database. Workload-type browsing shows the live game-server surface
and honestly marks the other kinds "later" (engine support is
M16). The customization entry point is the theme panel (§12's
Themes row). Successful create/import opens the new server's
console — the §9 end-to-end flow.

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

**M22 status:** the tab strip, tab state indicators, animations,
address bar, session restore, and context menu rows are implemented
(`zim/frontend/src/app.ts` + `tabs.ts` + `omnibox.ts` + `zimurl.ts`,
with `tests/` covering the pure logic). One honest scope note: tear-off
opens the tab in a new window where `window.open` is available
(plain browsers); inside the desktop shell it reports that
multi-window lands with M23, which owns native window management.

**M23 status:** the split view (§8), the NTP layout grammar (§9),
themes (the Themes row), and native tear-out/merge are implemented
(`src/split.ts` + `src/theme.ts` + `src/instanceForm.ts` + the v2
session in `tabs.ts` + the rewritten NTP + the split/multi-window
`app.ts`; shell window registry + tear-off/merge commands; engine
`/me/recents` + the IPC CSP sources). Remaining polish, honestly:
split-all, cross-window drag-merge, and the packaged-shell visual
fidelity pass (M25's E2E is where window flows get pressed).

## 13. Open integration questions

1. ~~Shell content hosting strategy~~ — **RESOLVED (M22, AD-24):
   single webview, iframe panes.** The chrome (`zim://app`) hosts
   tab content as same-origin iframes onto the engine's page
   surface; the shell keeps exactly one webview window. Rationale:
   panes inherit the pages' own view-only semantics and CSP, the
   frontend works unchanged in plain browsers, and the memory cost
   of one webview is flat. Native multi-webview (per-window
   fidelity, tear-out/merge) remains the M23 multi-window design's
   starting point — the iframe baseline is what it generalizes.
2. ~~`zim://` resolution~~ — **RESOLVED (M21, AD-23): loopback-origin
   mapping.** The shell rewrites `zim://` URLs onto the engine's
   `https://127.0.0.1:{port}/zim/…` origin at navigation time
   (`zim/shell/src/zim_url.rs`; the full §3 inventory is covered by
   its test suite). A shell custom protocol was rejected: it would
   make the shell proxy engine traffic — violating §2's "the shell
   never proxies engine traffic" — and it would break the
   frontend's same-origin design (the HttpOnly `zim_session`
   cookie and the `zim-events.v1` WebSocket sub-protocol) or force
   re-engineering both. `zim://` URLs remain stable *identifiers*;
   the loadable URL is derived at navigation time.
3. Webview divergence floor — **M22 answer:** the frontend's
   platform needs are ES2020 modules, WebSocket, Pointer Events,
   Constructable Stylesheets, `conic-gradient`, and CSS custom
   properties — the dependency-free surface stays the mitigation,
   and CI verifies the bytes (type-check, unit tests, fresh dist)
   rather than webview matrix behavior, which needs the packaged
   app. **M25 resolution:** the matrix is covered in layers (AD-30)
   — the Linux packaged-app smoke under Xvfb drives real WebKitGTK
   on every tagged build; WebView2 Evergreen and macOS WKWebView
   live-driving is M26 release validation, recorded in the
   roadmap rather than claimed here.
4. Shell auto-update channel mechanics (deferred to M25
   packaging). **M25 resolution:** delivered as AD-31 — the
   consent-shaped GitHub-Releases channel with minisign
   signatures; the normative contract is
   `docs/architecture/UPDATES.md`.

### M21 resolution notes (recorded for M22)

- **Engine discovery**: the engine writes `engine.json`
  (`schema`, `pid`, `port`, `engine_version`, `started_at_unix`)
  into its data dir at boot and removes it on graceful shutdown
  (`zim/engine/crates/main/src/discovery.rs`); the shell reads it,
  probes `/api/v1/health` over loopback TLS, attaches when healthy,
  and spawns a **detached** engine when nothing answers. A stale
  file (crashed engine) is a hint, not an error — the probe decides.
- **Loopback TLS trust**: the engine's certificate is persistent
  (`loopback-cert.pem` in the data dir); the shell installs it into
  the OS trust store on first run / rotation
  (`zim/shell/src/trust.rs`): per-user Root via `certutil` on
  Windows, the login keychain via `security` on macOS, and
  `/usr/local/share/ca-certificates` + `update-ca-certificates` on
  Linux (an unprivileged Linux shell surfaces the two elevated
  commands on its recovery page). TLS validation is disabled
  nowhere; the shell's probe verifier is a separate,
  loopback-dialed-only liveness check, not a webview setting.
- **Known first-run caveat (macOS)**: keychain trust added by
  `security add-trusted-cert` may require a shell relaunch before
  WKWebView honors it; the recovery page's Retry covers the gap.
- **Shell window model**: M21 ships one webview window (`main`) —
  the tab strip, address bar, split views, and multi-window
  behavior are M22/M23 shell-UI work layered on this baseline.

### M22 resolution notes (recorded for M23)

- **The chrome is a frontend surface** (`zim://app`, AD-24): the
  tab strip, omnibox, and panes are the frontend's own TypeScript
  (`zim/frontend/src/app.ts`), served by the engine like every
  other page. The shell's default attach target changed from
  `zim://newtab` to `zim://app`; `--open` still deep-links.
- **Panes are same-origin iframes**: the engine's page CSP gained
  `frame-src 'self'` and `frame-ancestors 'self'` with
  `X-Frame-Options: SAMEORIGIN` — same-origin framing in both
  directions, cross-origin embedding still denied (and the loopback
  host guard already confines who reaches the origin at all).
- **Indicator wiring**: the chrome holds one §11 events stream per
  server tab (the §9 events-stream slot); `instance.state_change`
  drives the tab dot, `instance.install_progress` drives the §4
  progress ring, gaps re-fetch state over HTTP, and a fatal stream
  (anonymous or permission-denied) degrades the dot with one HTTP
  fallback rather than breaking the tab.
- **Session format**: `zim.session.v1` — `{v: 1, tabs: [zim://
  paths], active: index}` — written debounced on every tab-set
  change and flushed on unload; restoration re-opens views only
  (invariants 3–6), and `?tab=` deep links (tear-off targets)
  replace the session like Chrome opening with a URL argument.
- **Tray "New tab"**: the shell navigates the main window to
  `/zim/app?newtab=1` (built directly — the `zim://` identifier
  grammar deliberately has no query strings, AD-23); the chrome
  restores the session plus one new tab. The reload cost is the
  honest trade: panes re-fetch their own data; an in-place event
  channel is M23 window work.
- **Fixed on the way**: the committed compiled assets carried
  extensionless import specifiers (`from "./api"`), which no real
  ES-module host resolves — tsc emits specifiers verbatim, and no
  browser-level execution had ever exercised the pages. The build
  now post-processes the emitted files (`scripts/fix-imports.mjs`)
  and the freshness gate in CI catches drift.

### M23 resolution notes (recorded for M24/M25)

- **Multi-window without session IPC**: torn-out tabs are real
  shell windows (`chrome-{n}` labels) pointed at
  `/zim/app?window={label}`; each chrome window persists its tab
  set under a per-window session key on the engine origin
  (`zim.session.v2.w:{label}`), so windows never clobber each
  other and the main window keeps the primary key. The shell
  persists only the *window set* (`windows.json` in its app-data
  dir; corrupt = empty) and recreates the windows after attach —
  each page restores its own session. No tab state crosses the
  IPC boundary; sessions stay view-only data in the frontend.
- **The chrome→shell channel**: the engine's page CSP gained
  `ipc:` and `http://ipc.localhost` in `connect-src` (Tauri's IPC
  schemes) so the chrome page can invoke the shell's four commands
  — `zim_tear_off`, `zim_merge_to_main`, `zim_new_window`,
  `zim_window_label`. The shell resolves every `zim://` path
  through the identifier grammar (AD-23) before building a window
  URL, so a hostile page cannot aim a window at an arbitrary
  target; the schemes terminate in the shell process (no network
  reach). In plain browsers the bridge is absent and every caller
  degrades (tear-off falls back to `window.open`, merge never
  appears).
- **Tear-off formula (AD-24 realized)**: `zim_tear_off` builds
  `/zim/app?tab={primary}&split={secondary}&ratio={r}&window={label}`
  — a split tab's pairing survives the new window; the ratio is
  clamped engine-side of the grammar (0.2–0.8) by the receiving
  chrome, which validates every path through the same restore
  discipline as its storage.
- **Merge**: a torn-off window's menu item hands its whole v2
  session to `zim_merge_to_main`; the shell relays it to the main
  window as a `zim:merge-session` event (payload validated by the
  same `restoreSession` — corrupt input is ignored, never fatal)
  and closes the window. Cross-window *drag* merge stays polish.
- **The in-place new-tab channel**: the tray's "New tab" emits
  `zim:new-tab` to the main window's chrome (no reload — panes
  keep their streams and console buffers); the `?newtab=1`
  navigation remains the fallback when the emit fails. The tray
  also gained "New window".
- **Themes are CSSOM, not markup**: live application writes CSS
  custom properties onto `:root` through the CSSOM (not style
  attributes — `style-src 'self'` is untouched), and the components'
  shadow-root styles were re-tokenized to `var(--…, fallback)` so
  custom properties inherit through the shadow boundaries. The
  vocabulary contract is `zim/contracts/theme-tokens.v1.json`;
  packaged-theme upload stays with M24's addon system.
- **Recents are engine state (§11 honored)**: `GET/POST
  /me/recents` (user-scoped, capped at the newest 32 per user,
  404 on unknown instances, unaudited — an open is nicety state).
  The chrome records pane adoptions fire-and-forget; stale rows
  for deleted instances are filtered by the NTP's merge with the
  visible instance list.
- **Fixed on the way**: `restoreTheme` now omits the `accent` key
  when unset (byte-stable round-trips); `clampRatio` maps
  degenerate drag math to the centered fallback rather than NaN.
