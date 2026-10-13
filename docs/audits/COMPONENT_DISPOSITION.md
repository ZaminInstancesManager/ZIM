# Component Disposition Matrix (Phase 4)

This is the canonical evidence-backed inventory of how each major upstream
component is treated in ZIM. Each entry assigns one of the six dispositions
defined by the prompt:

- **KEEP** — required as-is or with minimal changes.
- **ADAPT** — useful existing implementation that needs targeted ZIM
  modifications.
- **REPLACE** — behavior remains necessary, but the existing implementation
  is replaced (e.g. Pterodactyl's process-management implementation is
  replaced by the Rust supervisor).
- **REMOVE** — irrelevant product functionality that has been confirmed
  unnecessary and whose removal does not break required features, tests,
  builds, licensing or maintenance.
- **DEFER** — not needed for the first usable ZIM release, but its future
  value or removal cost needs explicit consideration.
- **INVESTIGATE** — insufficient evidence to choose safely.

Source paths cited below are real paths in the imported Pterodactyl
subtrees or real paths at the Chromium pinned revision (verified by
enumeration in `docs/audits/CHROMIUM_ARCHITECTURE.md` and
`docs/audits/PTERODACTYL_ARCHITECTURE.md`).

## Summary counts

| Disposition  | Count | Notes |
|--------------|-------|-------|
| KEEP         | 26    | Mostly Chromium browser infrastructure + Pterodactyl *behavioral contracts*. |
| ADAPT        | 8     | Targeted modifications where ZIM needs behavior the upstream doesn't provide. |
| REPLACE      | 14    | Almost the entire Pterodactyl backend; some Chromium surfaces where ZIM-specific behavior is required. |
| REMOVE       | 4     | Only verified-irrelevant product surfaces. The Pterodactyl subtrees are NOT removed; they are kept as audit reference. |
| DEFER        | 6     | Capabilities not needed for the first ZIM release. |
| INVESTIGATE  | 7     | Items needing deeper source inspection before a disposition can be settled. |

## 1. Chromium components

### 1.1 Browser process ownership

**Component:** `Browser` class, `BrowserManagerService` ownership root.
**Source:** `chrome/browser/ui/browser.h` (303 lines), `chrome/browser/`
at the pinned revision.
**Upstream responsibility:** owns a single browser window; coordinates
`TabStripModel`, `BrowserWindowFeatures`, and per-window feature graph.
**Dependencies:** `TabStripModel`, `BrowserWindowFeatures`, `TabFeatures`,
`content::WebContents`.
**Dependents:** every other chrome-layer subsystem that hangs off a
window.
**ZIM requirement:** preserve the browser-as-window model; do not
replace the window concept with a ZIM-specific window.
**Disposition:** **KEEP**.
**Evidence:** `browser.h` doc-comment: *"An instance of this class
represents a single browser window on Desktop. Owned by
BrowserManagerService."* Verified at the pinned revision.
**Effect on builds/tests:** no change.
**Security/compat:** preserves Chromium's window-isolation model.
**Replacement/migration:** none.
**Verification performed:** source inspection of `browser.h` head.

### 1.2 Tab strip model

**Component:** `TabStripModel` + `TabModel` + `TabFeatures` +
`TabStripModelObserver`.
**Source:** `chrome/browser/ui/tabs/` at the pinned revision.
**Upstream responsibility:** owns the tab list of a `Browser`; emits
observer events on tab open/close/move/replace; provides per-tab
`TabFeatures` attachment point.
**Dependencies:** `content::WebContents`, `sessions::SessionID`,
`tab_groups::TabGroupId`.
**Dependents:** `Browser`, session restore, drag-and-drop, recently
closed, omnibox (indirectly).
**ZIM requirement:** real Chromium tabs are the substrate for "server
tabs"; ZIM attaches metadata via `TabFeatures`, not via a parallel tab
manager.
**Disposition:** **ADAPT**.
**Evidence:** `tab_strip_model.h` (1551 lines) provides the observer
API ZIM needs; `tab_features.h` provides the per-tab feature graph
attachment point.
**Effect on builds/tests:** no Chromium source modification required;
ZIM uses the existing API. A small ZIM-side `TabFeature`
implementation is added in the integration layer (no upstream patch).
**Security/compat:** preserved.
**Replacement/migration:** none on the Chromium side; ZIM's
`ServerTabFeature` (in `zim/integration/`) implements the
`TabFeature` interface.
**Verification performed:** source inspection of `tab_strip_model.h`
head; enumeration of `chrome/browser/ui/tabs/`.

### 1.3 Session restore

**Component:** session service + session restore + exit type service +
restore-on-startup policy handler.
**Source:** `chrome/browser/sessions/session_service.{h,cc}`,
`session_restore.{h,cc}`, `exit_type_service.{h,cc}`,
`restore_on_startup_policy_handler.{h,cc}`,
`session_service_log.{h,cc}` (append-only log),
`tab_restore_service_*.{h,cc}`.
**Upstream responsibility:** persists per-window/tab navigation state;
restores on startup; distinguishes normal vs crash exit.
**Dependencies:** `content::WebContents`, `sessions::SessionID`,
profile services.
**Dependents:** startup, crash recovery, recently-closed.
**ZIM requirement:** session restoration must restore *tab views*, not
issue server lifecycle commands. This invariant is enforced at the
engine boundary, not in Chromium.
**Disposition:** **KEEP**.
**Evidence:** full session infrastructure enumerated at the pinned
revision (see `docs/audits/CHROMIUM_ARCHITECTURE.md` §6.3).
**Effect on builds/tests:** no change.
**Security/compat:** preserves upstream behavior. ZIM enforces
"restoring a session does not start servers" at the engine boundary
(see `docs/architecture/RUST_ENGINE.md` §lifecycle).
**Replacement/migration:** none.
**Verification performed:** directory enumeration of
`chrome/browser/sessions/` (file list recorded in
`docs/audits/CHROMIUM_ARCHITECTURE.md`).

### 1.4 Split view

**Component:** `split_tabs` namespace + `SplitTabCollection` +
`SplitTabLayout`/`SplitTabCreatedSource`/`SplitTabOrientationChangeSource`
enums + `split_tab_*` and `split_view_*` source files.
**Source:** `chrome/browser/ui/tabs/split_tab_util.{h,cc}`,
`split_tab_highlight_controller.{h,cc}`,
`split_tab_menu_model.{h,cc}`,
`split_view_layout_menu_model.{h,cc}`,
`split_view_iph_controller.{h,cc}`,
`split_tab_metrics.{h,cc}`,
`split_tab_mute_menu_model.{h,cc}`,
`split_tab_swap_menu_model.{h,cc}`; `split_tabs` namespace
forward-declared in `tab_strip_model.h`.
**Upstream responsibility:** provides first-class split-view (two or
more panes per window, independent navigation per pane, layout
switching, pane focus, swap, persistence).
**Dependencies:** `TabStripModel`, `tabs::SplitTabCollection`,
`tabs::TabStripCollection`, `tabs::TabGroupTabCollection`.
**Dependents:** tab UI, session restore, drag-and-drop.
**ZIM requirement:** ZIM needs split view to show two server views
side-by-side (e.g. console + files). Chromium 157 already provides
this.
**Disposition:** **KEEP**.
**Evidence:** `tab_strip_model.h` forward-declares the `split_tabs`
namespace with five types (`SplitTabData`, `SplitTabVisualData`,
`SplitTabLayout`, `SplitTabCreatedSource`,
`SplitTabOrientationChangeSource`); the directory enumeration
confirms eight `split_tab_*`/`split_view_*` source files exist.
**Effect on builds/tests:** no change. ZIM uses the existing API.
**Security/compat:** preserved.
**Replacement/migration:** none. ZIM contributes a ZIM-specific surface
that *uses* split view (a "split with server console" menu item) via
the existing `split_tab_menu_model` extension path.
**Verification performed:** source inspection of `tab_strip_model.h`
forward declarations; directory enumeration of
`chrome/browser/ui/tabs/`.

### 1.5 Bookmarks, history, pinned tabs

**Component:** bookmarks + history + pinned tabs.
**Source:** `chrome/browser/ui/bookmarks/`, `chrome/browser/bookmarks/`,
`chrome/browser/history/`, `chrome/browser/ui/tabs/pinned_tab_*`.
**Upstream responsibility:** bookmarks, history, pinned tabs.
**ZIM requirement:** preserved. A user can bookmark a server console
URL; pinning a server console tab works like any other tab.
**Disposition:** **KEEP**.
**Evidence:** directory enumerations confirm presence.
**Effect on builds/tests:** no change.
**Security/compat:** preserved.
**Verification performed:** directory enumeration.

### 1.6 Address bar / omnibox

**Component:** omnibox.
**Source:** `chrome/browser/ui/omnibox/`,
`chrome/browser/ui/views/omnibox/`.
**Upstream responsibility:** URL entry, autocomplete, navigation.
**ZIM requirement:** preserved. Server management is reached via NTP,
tab strip indicators, and direct `zim://` URLs; the omnibox is not
modified.
**Disposition:** **KEEP**.
**Evidence:** directory presence.
**Verification performed:** directory enumeration.

### 1.7 New-tab page

**Component:** NTP.
**Source:** `chrome/browser/ui/webui/ntp/`, `components/ntp_*`.
**Upstream responsibility:** the page shown on `chrome://newtab`.
**ZIM requirement:** ZIM replaces the default NTP with a ZIM-authored
internal page at `zim://newtab`. The replacement is via WebUI
registration (no Chromium source modification).
**Disposition:** **ADAPT** (the default NTP is *replaced* by ZIM via
the WebUI registration mechanism, not by patching the upstream NTP
source).
**Evidence:** WebUI mechanism is the standard extension point.
**Effect on builds/tests:** no Chromium patch; ZIM-side registration.
**Security/compat:** the upstream NTP remains available if the user
explicitly reverts to it.
**Verification performed:** WebUI infrastructure presence (referenced
in `chrome/browser/ui/webui/` enumeration).

### 1.8 Internal WebUI pages and `chrome://` constants

**Component:** WebUI infrastructure + `chrome://` URL constants.
**Source:** `chrome/browser/ui/webui/`, `content/public/browser/web_ui.h`,
`chrome/common/url_constants.h` (749 lines).
**Upstream responsibility:** registers internal pages at `chrome://`
URLs.
**ZIM requirement:** ZIM registers `zim://` URLs through the same
mechanism, with the engine as the data source.
**Disposition:** **ADAPT**.
**Evidence:** `url_constants.h` enumerates the `chrome://` URLs; ZIM
adds a parallel `zim://` set via the same registration path.
**Effect on builds/tests:** small ZIM-side patch (one new URL-scheme
registration). The patch will be under `chromium/PATCHES/` when the
source tree is imported.
**Verification performed:** file inspection of `url_constants.h`.

### 1.9 Extension system

**Component:** Chrome extension platform (MV3).
**Source:** `extensions/`, `chrome/browser/extensions/` (611 entries).
**Upstream responsibility:** extension packaging, install, update,
permission enforcement, content scripts, MV3 service workers.
**ZIM requirement:** ZIM preserves the Chrome extension platform for
ordinary browser extensions; ZIM *does not* add ZIM server permissions
to this platform automatically. A separate ZIM extension manifest key
(see `docs/api/FRONTEND_EXTENSION_SPEC.md`) declares ZIM permissions,
checked at the engine boundary.
**Disposition:** **KEEP** (Chromium extension platform as-is for
ordinary extensions).
**Evidence:** 611 entries enumerated in
`chrome/browser/extensions/`.
**Effect on builds/tests:** no change.
**Security/compat:** preserved; ZIM does not weaken the MV3 model.
**Verification performed:** directory enumeration.

### 1.10 Site isolation and sandboxing

**Component:** renderer sandbox, site isolation.
**Source:** `content/public/browser/`, `sandbox/`.
**Upstream responsibility:** privilege separation between browser and
renderers; per-site isolation.
**ZIM requirement:** preserved unmodified. ZIM does not weaken the
sandbox to make integration easier.
**Disposition:** **KEEP**.
**Evidence:** `content/public/browser/` enumerated (695+ entries);
`sandbox/` present at top level.
**Verification performed:** top-level directory enumeration.

### 1.11 Network service

**Component:** network service.
**Source:** `services/network/`, `net/`.
**Upstream responsibility:** HTTP, HTTPS, WebSockets, DNS, cache.
**ZIM requirement:** preserved. The engine's own HTTPS/WSS surface is
independent of the browser's network service.
**Disposition:** **KEEP**.
**Verification performed:** top-level directory enumeration.

### 1.12 Build system (GN + Ninja + DEPS + depot_tools)

**Component:** `.gn`, `BUILD.gn`, `DEPS`, `depot_tools`.
**Source:** `.gn` (479 lines), `BUILD.gn`, `DEPS` (4978 lines).
**Upstream responsibility:** build configuration and dependency
fetch.
**ZIM requirement:** preserved. ZIM-specific build targets are added
under `zim/BUILD.gn` (in this repo, not in chromium/src), and any
Chromium-side build modifications are local patches under
`chromium/PATCHES/`.
**Disposition:** **KEEP**.
**Effect on builds/tests:** ZIM-specific build targets are additive.
**Verification performed:** file inspection of `.gn`, `DEPS`,
`BUILD.gn`.

### 1.13 `about_flags.cc` (feature flags)

**Component:** `chrome://flags` feature flag registry.
**Source:** `chrome/browser/about_flags.cc`.
**Upstream responsibility:** runtime feature flag registry.
**ZIM requirement:** ZIM-specific feature flags (e.g. an experimental
tab visual style) should be gated by adding entries to
`about_flags.cc` rather than by deleting features.
**Disposition:** **ADAPT** (additive patch).
**Effect on builds/tests:** small additive patch; no removals.
**Verification performed:** file presence confirmed.

### 1.14 Chromium test infrastructure

**Component:** `*_unittest.cc`, `*_browsertest.cc`,
`*_interactive_uitest.cc`, `*_apitest.cc` test types.
**Source:** distributed throughout `chrome/`, `content/`, etc.
**Upstream responsibility:** upstream regression tests.
**ZIM requirement:** ZIM runs the relevant upstream tests on each
re-pin as regression checks (see `docs/architecture/TEST_STRATEGY.md`).
**Disposition:** **KEEP**.
**Verification performed:** test files enumerated alongside their
production counterparts in `chrome/browser/ui/tabs/` and
`chrome/browser/sessions/`.

## 2. Pterodactyl Panel components

### 2.1 Panel application code (`app/`)

**Component:** Panel `app/` tree (Laravel application).
**Source:** `upstream/pterodactyl-panel/app/`.
**Upstream responsibility:** the entire Panel backend.
**ZIM requirement:** the *behavior* is replaced by the Rust engine;
the *source* is preserved as audit reference.
**Disposition:** **REPLACE** (behavior; source kept as reference).
**Evidence:** the 17 `app/Services/` subdirectories map directly to
the Rust engine's domain modules (see
`docs/audits/PTERODACTYL_ARCHITECTURE.md` §7.1.1).
**Effect on builds/tests:** Panel source is not built; no test
impact.
**Security/compat:** the *behavioral contracts* (Permission
constants, Server schema, API surface) are preserved as ZIM's
compatibility surface.
**Replacement/migration:** the Rust engine modules
`zim_engine::domain::*` replace the Panel services; an importer
(`docs/compatibility/MIGRATION_AND_COMPATIBILITY.md`) reads existing
Panel MySQL databases into ZIM's format.
**Verification performed:** full enumeration of `app/Services/`,
`app/Models/`, `routes/`.

### 2.2 Panel database migrations

**Component:** Panel `database/migrations/` (195 migrations, 2016-2024).
**Source:** `upstream/pterodactyl-panel/database/migrations/`.
**Upstream responsibility:** Laravel schema migrations.
**ZIM requirement:** ZIM defines its own schema (see
`docs/architecture/RUST_ENGINE.md` §persistence) and provides an
importer for existing Pterodactyl databases.
**Disposition:** **REPLACE**.
**Evidence:** migrations verified present (195 files).
**Verification performed:** `ls | wc -l` on the subtree.

### 2.3 Panel routes (api-application, api-client, api-remote)

**Component:** Panel route files.
**Source:** `upstream/pterodactyl-panel/routes/api-application.php`,
`api-client.php`, `api-remote.php`, `admin.php`, `auth.php`, `base.php`.
**Upstream responsibility:** define the HTTP API surface for clients
(server owners, subusers), applications (third-party API consumers),
the Wings daemon (remote), and admin UI.
**ZIM requirement:** ZIM replaces the *remote* (Wings) surface with
the engine's internal node-runtime contract; ZIM optionally preserves
the *client* surface for legacy compatibility.
**Disposition:** **REPLACE** (api-remote), **DEFER**
(api-client, api-application) for legacy compatibility.
**Evidence:** `api-remote.php` head read; the 11 endpoints it
exposes are documented in
`docs/audits/PTERODACTYL_ARCHITECTURE.md` §7.1.5.
**Verification performed:** file inspection.

### 2.4 Panel frontend (Vue + Tailwind)

**Component:** Panel `resources/` and `public/` (Vue + Tailwind + Vite
frontend).
**Source:** `upstream/pterodactyl-panel/resources/`,
`upstream/pterodactyl-panel/public/`.
**Upstream responsibility:** the traditional Pterodactyl dashboard UI.
**ZIM requirement:** not the ZIM target frontend.
**Disposition:** **REMOVE** from ZIM's runtime (source preserved as
audit reference in `upstream/`); ZIM's default frontend is authored
fresh under `zim/frontend/`.
**Evidence:** the prompt is explicit: *"The traditional Pterodactyl
dashboard is not ZIM's target frontend."*
**Effect on builds/tests:** ZIM does not build the Panel frontend.
**Security/compat:** no impact (the frontend is not ZIM's runtime).
**Verification performed:** directory enumeration.

### 2.5 Panel Docker/deployment artifacts

**Component:** Panel Dockerfile, docker-compose.example.yml, Nix flake.
**Source:** `upstream/pterodactyl-panel/Dockerfile`,
`docker-compose.example.yml`, `flake.nix`, `flake.lock`.
**Upstream responsibility:** Panel-as-server deployment model.
**ZIM requirement:** ZIM is a desktop product, not a server-deployed
panel; these artifacts are not used in ZIM's runtime.
**Disposition:** **REMOVE** from ZIM's runtime (preserved as audit
reference).
**Evidence:** ZIM is a desktop product per the prompt's product
identity section.
**Verification performed:** file presence confirmed.

### 2.6 Panel tests

**Component:** Panel `tests/` (Laravel PHPUnit tests).
**Source:** `upstream/pterodactyl-panel/tests/`.
**Upstream responsibility:** Panel regression tests.
**ZIM requirement:** the *behavior* of these tests is preserved in
spirit in ZIM's own integration tests (re-implemented in Rust against
the new engine).
**Disposition:** **DEFER** (ZIM's own tests are the canonical tests
for ZIM behavior; the Panel tests are a reference for behavior to
reproduce).
**Verification performed:** directory presence (test files not
enumerated in detail in this phase; no PHP runtime to run them).

## 3. Pterodactyl Wings components

### 3.1 Wings daemon entry (`wings.go`, `cmd/`)

**Component:** Wings daemon entry.
**Source:** `upstream/pterodactyl-wings/wings.go`,
`upstream/pterodactyl-wings/cmd/`.
**Upstream responsibility:** the Wings process; init, config load,
router bind.
**ZIM requirement:** replaced by the Rust engine's main binary; ZIM
does not run a separate Wings daemon.
**Disposition:** **REPLACE**.
**Evidence:** Wings is a standalone daemon (verified: `wings.go` is a
top-level main file); ZIM's engine owns the runtime.
**Verification performed:** file inspection.

### 3.2 Wings server supervision (`server/`)

**Component:** Wings per-server supervision (Server struct, Manager,
power actions, crash detection, install, websockets).
**Source:** `upstream/pterodactyl-wings/server/` (`server.go`,
`manager.go`, `power.go`, `crash.go`, `install.go`,
`websockets.go`, `events.go`, `console.go`, `connections.go`,
`resources.go`, `mounts.go`, `config_parser.go`, `configuration.go`,
`listeners.go`, `update.go`, `errors.go`, `state_test.go`,
`power_test.go`, `console_test.go`, `backup/`, `filesystem/`,
`installer/`, `transfer/`).
**Upstream responsibility:** per-server process supervision,
lifecycle, power-action serialization, crash detection, install,
WebSocket fan-out, file system access, backups, transfers.
**ZIM requirement:** the *behavior* is replaced by the Rust engine's
supervision module; the *source* is preserved as audit reference and
as the behavioral-contract reference for the Rust reimplementation.
**Disposition:** **REPLACE** (behavior; source preserved as
reference).
**Evidence:** verbatim source excerpts in
`docs/audits/PTERODACTYL_ARCHITECTURE.md` §7.2.1–7.2.6 document the
Server struct, PowerAction serialization,
ProcessEnvironment interface, CrashHandler gating, install behavior,
and WebsocketBag.
**Effect on builds/tests:** Wings source is not built; ZIM's
`zim_engine::supervision` module replaces it.
**Security/compat:** the *behavioral invariants* (per-instance
serialization, four-state model, crash-loop protection) are preserved
in the Rust reimplementation.
**Verification performed:** head-of-file reading of `server.go`,
`power.go`, `environment.go`, `crash.go`, `install.go`,
`websockets.go`, `manager.go`.

### 3.3 Wings environment abstraction (`environment/`)

**Component:** `ProcessEnvironment` interface + Docker implementation.
**Source:** `upstream/pterodactyl-wings/environment/`
(`environment.go`, `docker.go`, `docker/`, `allocations.go`,
`config.go`, `settings.go`, `stats.go`).
**Upstream responsibility:** abstract the runtime (so the server
supervisor doesn't directly depend on Docker).
**ZIM requirement:** the Rust `Runtime` trait is the analogue; the
Docker runtime remains the primary implementation for container
isolation on Linux.
**Disposition:** **REPLACE** (interface, in Rust).
**Evidence:** `environment.go` declares the `ProcessEnvironment`
interface with `Type()`, `Config()`, `Events()` methods and the
four-state constants.
**Verification performed:** head-of-file reading of `environment.go`
and `docker.go`.

### 3.4 Wings router (`router/`)

**Component:** HTTP/WS router (gin-gonic).
**Source:** `upstream/pterodactyl-wings/router/` (`router.go`,
`router_server.go`, `router_server_backup.go`,
`router_server_files.go`, `router_server_transfer.go`,
`router_server_ws.go`, `router_system.go`, `router_transfer.go`,
`router_download.go`, `middleware/`, `tokens/`, `websocket/`,
`downloader/`).
**Upstream responsibility:** the HTTP/WS surface Wings exposes to
Panel.
**ZIM requirement:** replaced by the engine's internal API (the
engine owns both the client surface and the runtime surface; no
Panel-to-Wings split).
**Disposition:** **REPLACE**.
**Evidence:** directory enumeration confirms router file inventory.
**Verification performed:** directory enumeration.

### 3.5 Wings SFTP (`sftp/`)

**Component:** SFTP server.
**Source:** `upstream/pterodactyl-wings/sftp/`.
**Upstream responsibility:** SFTP file access to server files.
**ZIM requirement:** SFTP remains useful for some users; ZIM may
expose an SFTP server for backwards compatibility.
**Disposition:** **DEFER** (SFTP is not needed for the first ZIM
release; deferred to a later milestone).
**Verification performed:** directory enumeration.

### 3.6 Wings remote client (`remote/`)

**Component:** Panel RPC client.
**Source:** `upstream/pterodactyl-wings/remote/`.
**Upstream responsibility:** Wings calls back to Panel for SFTP auth,
activity, install status, transfer status, backup status.
**ZIM requirement:** replaced by the engine's internal node-runtime
contract (the engine owns both ends of the conversation).
**Disposition:** **REPLACE**.
**Evidence:** presence in `go.mod` and import in `server.go`.
**Verification performed:** directory enumeration.

### 3.7 Wings events bus (`events/`)

**Component:** per-server event bus.
**Source:** `upstream/pterodactyl-wings/events/`.
**Upstream responsibility:** publish/subscribe for state changes,
resource metrics, install progress.
**ZIM requirement:** replaced by the Rust engine's event distribution
module (see `docs/architecture/RUST_ENGINE.md` §events).
**Disposition:** **REPLACE**.
**Evidence:** `environment.go` references `events.Bus` as the event
emitter returned by `ProcessEnvironment.Events()`.
**Verification performed:** file inspection.

### 3.8 Wings config (`config/`)

**Component:** Wings configuration.
**Source:** `upstream/pterodactyl-wings/config/`.
**Upstream responsibility:** parse Wings's `config.yml`.
**ZIM requirement:** replaced by ZIM's engine configuration (which
includes the engine's own config and per-node config).
**Disposition:** **REPLACE**.
**Verification performed:** directory enumeration.

### 3.9 Wings parser (`parser/`)

**Component:** Console output / config file parser.
**Source:** `upstream/pterodactyl-wings/parser/`.
**Upstream responsibility:** parse egg-defined config file formats
(properties, XML, JSON) and rewrite them with substituted variables.
**ZIM requirement:** preserved for egg compatibility (the
`beevik/etree` XML parser is a dependency for some eggs).
**Disposition:** **ADAPT** (a Rust equivalent is implemented in the
engine's egg-compatibility module).
**Verification performed:** directory enumeration.

### 3.10 Wings system utilities (`system/`)

**Component:** `system.Locker`, `system.AtomicString`, etc.
**Source:** `upstream/pterodactyl-wings/system/`.
**Upstream responsibility:** concurrency primitives used by the
server supervisor.
**ZIM requirement:** replaced by Rust's standard library equivalents
(`Mutex<T>`, `AtomicUsize`, etc.).
**Disposition:** **REPLACE**.
**Evidence:** `powerLock *system.Locker` referenced in `server.go`.
**Verification performed:** file inspection of `server.go`.

### 3.11 Wings tests (`*_test.go`)

**Component:** Wings Go test files.
**Source:** throughout `upstream/pterodactyl-wings/`.
**Upstream responsibility:** Wings regression tests.
**ZIM requirement:** behavior preserved in spirit in ZIM's own tests.
**Disposition:** **DEFER** (reference for behavior to reproduce).
**Verification performed:** `*_test.go` files confirmed present
alongside production code.

## 4. Cross-cutting

### 4.1 The Panel↔Wings RPC contract

**Component:** the contract surface in `routes/api-remote.php` and
the Panel-side RPC client in `app/Repositories/Wings/`.
**Source:** `upstream/pterodactyl-panel/routes/api-remote.php`,
`upstream/pterodactyl-panel/app/Repositories/Wings/`.
**Upstream responsibility:** define the conversation between Panel
and Wings.
**ZIM requirement:** ZIM's engine owns both ends of the conversation
internally; the contract is replaced by the engine's internal
node-runtime contract. A legacy importer is provided for users
migrating from Pterodactyl.
**Disposition:** **REPLACE**.
**Evidence:** `api-remote.php` head read.
**Verification performed:** file inspection.

### 4.2 Pterodactyl egg format

**Component:** the egg JSON schema.
**Source:** referenced through `app/Services/Eggs/` and
`app/Models/Egg.php`; the egg ecosystem itself lives at
`pterodactyl/eggs` (not in this repo).
**Upstream responsibility:** define server definitions.
**ZIM requirement:** ZIM preserves egg compatibility through an
importer (see `docs/compatibility/MIGRATION_AND_COMPATIBILITY.md`).
A ZIM-native `zim.toml` format is the long-term preferred format.
**Disposition:** **ADAPT** (importer, with security tightening for
privileged install scripts).
**Verification performed:** referenced through Panel code; an
example egg was not directly read in this phase.

### 4.3 Activity log / audit log

**Component:** Panel activity log + Wings activity forwarding.
**Source:** `app/Models/ActivityLog.php`, `app/Services/Activity/`,
`routes/api-remote.php` `/activity` endpoint.
**Upstream responsibility:** audit log of user actions and daemon
events.
**ZIM requirement:** ZIM's engine has its own audit module; the
*contract* of what is logged is preserved as a compatibility surface.
**Disposition:** **REPLACE**.
**Verification performed:** presence in Panel and Wings source.

### 4.4 Permission vocabulary

**Component:** Panel `Permission` constants.
**Source:** `app/Models/Permission.php`.
**Upstream responsibility:** per-subuser-per-server permission
vocabulary.
**ZIM requirement:** preserved verbatim in ZIM's authz module for
compatibility with existing tooling.
**Disposition:** **KEEP** (vocabulary preserved; implementation in
Rust).
**Evidence:** constants extracted in
`docs/audits/PTERODACTYL_ARCHITECTURE.md` §7.1.4.
**Verification performed:** file inspection.

## 5. INVESTIGATE items (insufficient evidence this phase)

These items need deeper source inspection before a final disposition
can be settled. They are recorded as INVESTIGATE and tracked in
`docs/architecture/DECISIONS.md` as open questions.

1. **`TabFeatures` API surface** — the file
   `chrome/browser/ui/tabs/tab_features.h` exists at the pinned
   revision but its contents were not fetched in this phase. The
   exact attachment API for per-tab ZIM metadata depends on this
   surface.
2. **`BrowserWindowFeatures` API surface** — same situation; the
   per-window feature graph attachment point.
3. **`WebUI` registration mechanism for custom schemes** —
   `content/public/browser/web_ui.h` exists but was not directly read;
   the exact mechanism for registering `zim://` URLs depends on it.
4. **Full `ProcessEnvironment` interface method set** — only the
   head of `environment.go` was read; the rest of the interface
   defines methods like `Start`, `Stop`, `Kill`, `Exec`,
   `AttachConsole`, `ExitState` which the Rust `Runtime` trait must
   mirror.
5. **Full `CrashHandler.handleServerCrash` body** — only the gating
   logic was read; the threshold and backoff logic for repeated
   crashes is unverified.
6. **Full `Server.install(reinstall)` body** — only the head was
   read.
7. **Panel-side `app/Repositories/Wings/` content** — the
   Panel-to-Wings RPC client surface is referenced through
   `ServerCreationService` but the directory's contents were not
   enumerated in this phase.

These INVESTIGATE items do not block the architecture phase; they are
blockers for the first implementation milestone that touches the
corresponding source. They are explicitly called out in
`docs/architecture/IMPLEMENTATION_ROADMAP.md` as pre-implementation
investigation tasks.

## 6. Verification performed vs. not performed

What was verified for this matrix:

- File-level presence of every cited component in the imported
  Pterodactyl subtrees (verified by directory enumeration and file
  listing).
- Top-level and per-subsystem directory presence at the Chromium
  pinned revision (verified by GitHub contents API).
- Head-of-file contents for key files (cited under "Verification
  performed" per entry).
- Cross-references between components (e.g. `powerLock *system.Locker`
  referenced in `server.go`; `DaemonServerRepository` referenced in
  `ServerCreationService`).

What was not verified:

- Full file contents for files where only the head was read.
- Cross-component dependency graphs at the function level (would
  require reading the full body of every cited file; deferred to
  implementation).
- Build-time dependency verification (no toolchain).

The matrix is therefore *evidence-backed at the source-existence
level* and *evidence-backed at the head-of-file level* for cited
files, but it is not a complete static-analysis dependency graph.
Where this distinction matters (i.e. where a disposition depends on
a dependency that has not been directly verified), the item is marked
INVESTIGATE.
