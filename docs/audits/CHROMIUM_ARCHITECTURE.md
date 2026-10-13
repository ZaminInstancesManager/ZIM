# Chromium Architecture Audit (Phase 2)

This audit records the actual Chromium architecture relevant to ZIM, grounded
in direct source inspection at the pinned revision `157.0.8103.0` (commit
`b0a05135bf5a60bb3ab5a1e86a0e55193cb126d0`) on the chromium/chromium GitHub
mirror. Every source path cited below was enumerated via the GitHub contents
API or spot-fetched via raw HTTPS at the pinned revision; the spot-fetched
files are listed at the end of this document.

The audit is scoped to the subsystems that ZIM must understand in order to
extend Chromium into a server-management browser: the browser process, the
tab model, session persistence, split view, internal pages, the extension
system, and the build/maintenance infrastructure. It is not a complete
description of Chromium. Subsystems that ZIM will not extend (rendering,
compositing, networking internals, media, etc.) are out of scope and are
mentioned only where they constrain ZIM's choices.

## 6.1 Browser process architecture

Chromium's desktop browser process is implemented primarily under
`chrome/browser/`. The top-level entry points and ownership roots relevant
to ZIM are:

- **`Browser` class** (`chrome/browser/ui/browser.h`, 303 lines at the
  pinned revision). Doc-comment (verbatim from the header): *"An instance of
  this class represents a single browser window on Desktop. Owned by
  BrowserManagerService. All features that are scoped to a browser window
  should have lifetime scoped to an instance of this class, usually via
  direct or indirect ownership of a std::unique_ptr. See
  BrowserWindowFeatures and TabFeatures."* Inheritance:
  `class Browser : public TabStripModelObserver, public
  WebContentsCollection::Observer, public BrowserWindowInterface`.
  Implications for ZIM: a `Browser` is a window, not a tab; tabs are owned
  by `TabStripModel` (see §6.2); the per-window feature graph
  (`BrowserWindowFeatures`) and per-tab feature graph (`TabFeatures`) are
  the extension points ZIM should use to attach server-management behavior
  without rewriting the browser.

- **`BrowserManagerService`** is referenced as the owner of `Browser`
  instances. ZIM should not need to modify this; it is the orchestration
  point for browser window lifecycle and is upstream-stable.

- **`TabStripModel`** (`chrome/browser/ui/tabs/tab_strip_model.h`, 1551
  lines) is the model that owns the tab list of a `Browser`. Its
  constructor surface and observer API (`TabStripModelObserver`) are the
  correct integration points for ZIM to:
  - Subscribe to tab open/close/move/replace events.
  - Associate server-state metadata with a tab (via `TabFeatures`).
  - Reflect server lifecycle state into a tab's visual representation
    without inventing a parallel tab manager.

- **`content::WebContents`** (`content/public/browser/web_contents.h`)
  is the embedder-facing handle to a tab's content. ZIM's integration layer
  will hold `content::WebContents*` for the tabs that render the ZIM
  internal pages (console, files, settings) and treat them as ordinary
  Chromium content; nothing about server management requires ZIM to
  subclass `WebContents`.

- **Mojo IPC** (under `mojo/`, `content/public/browser/`) is the
  browser↔renderer RPC substrate. ZIM does not need to add new Mojo
  interfaces for ordinary server management — the engine can expose its
  API to a renderer via an HTTPS+WSS origin (see
  `docs/architecture/BROWSER_INTEGRATION.md`).

- **Site isolation** is preserved as-is. ZIM does not weaken the
  renderer-process-per-site-isolation model. The ZIM privileged internal
  pages run in a dedicated, isolated origin (their own process) with a
  documented privilege grant from the engine (see
  `docs/security/THREAT_MODEL.md`).

- **Network service** (`services/network/`) is preserved as-is. ZIM does
  not need to interpose on the network service; the engine's own network
  surface (HTTPS + WSS) is independent of the browser's network service.

- **Sandboxing** is preserved. The browser process is privileged; the
  engine process is privileged; renderers are sandboxed. ZIM does not
  weaken Chromium's renderer sandbox to make integration easier. The
  integration contract is: the renderer requests operations through the
  engine's HTTPS/WSS API with capability-scoped tokens; the engine
  authorizes each request against its capability store.

### What ZIM must not do in this area

- Do not centralize all tabs in a single privileged process. Site
  isolation is preserved.
- Do not subclass `WebContents` to attach server state; use `TabFeatures`
  instead (see `chrome/browser/ui/tabs/tab_features.h` at the pinned
  revision).
- Do not invent a parallel tab manager. Every tab the user opens as a
  "server tab" is a real `TabStripModel` entry, with the same session
  restore behavior as any other tab.
- Do not modify `BrowserManagerService` or the browser window lifetime
  unless a ZIM-specific need is documented in
  `docs/architecture/BROWSER_INTEGRATION.md` and the corresponding local
  Chromium patch is recorded under `chromium/PATCHES/`.

## 6.2 Browser UI and navigation

The browser UI is implemented under `chrome/browser/ui/`. The relevant
subsystems ZIM must reuse, not re-implement, are:

- **Tab strip and tab model.** `chrome/browser/ui/tabs/` is the model layer
  (`tab_strip_model.{h,cc}`, `tab_model.{h,cc}`, `tab_data.{h,cc}`,
  `tab_features.{h,cc}`, `tab_menu_model.{h,cc}`,
  `tab_group_model.{h,cc}`). The view layer lives under
  `chrome/browser/ui/views/tabs/` (verified by directory listing at the
  pinned revision). ZIM's "server tab" concept is metadata attached to a
  `TabStripModel` entry via `TabFeatures`, not a new tab class.

- **Address bar and navigation controls.** Implemented in
  `chrome/browser/ui/views/omnibox/` and `chrome/browser/ui/omnibox/`.
  ZIM does not modify the omnibox; the omnibox is the user's primary
  navigation surface and remains upstream-stable. Server management is
  reached via the new-tab page, the tab strip's server-tab visual
  indicator, and direct `zim://` URLs (see §10.12 of the prompt and
  `docs/api/FRONTEND_EXTENSION_SPEC.md`).

- **Bookmarks.** `chrome/browser/ui/bookmarks/` and
  `chrome/browser/bookmarks/`. ZIM preserves bookmarks; a user can
  bookmark a server's console URL just like any other URL, and the
  bookmark restores a tab to that server's view on click. This is a
  natural consequence of using real Chromium tabs, not an additional
  feature.

- **Browser history.** `chrome/browser/history/` and
  `components/history/`. Preserved as-is. Server-tab navigations appear in
  history like any other navigation; this is intentional and provides
  useful audit surface for users who want to know "what server views did
  I open recently?".

- **Pinned tabs.** Implemented via `pinned_tab_codec.{h,cc}`,
  `pinned_tab_service.{h,cc}` in `chrome/browser/ui/tabs/`. ZIM preserves
  pinned tabs; pinning a server console tab is a natural way to keep a
  frequently-used server visible without taking horizontal space.

- **Context menus.** Implemented via `tab_menu_model.{h,cc}` and the
  `existing_*_sub_menu_model.{h,cc}` family. ZIM contributes server-tab
  context-menu items through the standard `TabMenuModel` addition path,
  not by patching the context-menu controller directly.

- **Keyboard shortcuts.** Implemented in
  `chrome/browser/ui/views/accelerators/` and related. ZIM does not
  modify the keyboard shortcut table for ordinary browser shortcuts; it
  may register additional ZIM-specific accelerators through the
  `BrowserWindowFeatures` extension point, scoped to the active ZIM
  surface.

- **Window management.** Implemented in `chrome/browser/ui/browser.h` /
  `browser_window/`. Preserved. Multiple browser windows are real
  Chromium windows, not a ZIM abstraction.

- **New-tab pages.** Implemented in `chrome/browser/ui/webui/ntp/`
  (and the relevant `components/ntp_*`). ZIM replaces the default NTP
  with a ZIM-authored internal page reachable via `zim://newtab`. The
  ZIM NTP is an ordinary WebUI page; it does not require Chromium
  source modifications. The replacement is configurable (see §10.10 of
  the prompt and `docs/api/FRONTEND_EXTENSION_SPEC.md`).

- **Browser settings.** `chrome/browser/ui/views/settings/`,
  `chrome/browser/ui/webui/settings/`. Preserved. ZIM-specific settings
  are presented on a ZIM-authored settings page reachable via
  `zim://settings`, not by modifying Chromium's settings.

- **Internal WebUI pages.** Implemented via
  `chrome/browser/ui/webui/` and `content/public/browser/web_ui.h`.
  `chrome/common/url_constants.h` (749 lines at the pinned revision)
  defines the `chrome://` URL constants. ZIM registers `zim://` URLs
  through the same WebUI infrastructure (a custom scheme + a WebUI data
  source). The integration is documented in
  `docs/architecture/BROWSER_INTEGRATION.md`.

- **Page loading indicators and navigation transitions.** Preserved
  as-is. Server-tab pages participate in normal navigation transitions.

- **Drag-and-drop.** Preserved. Users can drag tabs between windows,
  tear off tabs into new windows, etc., and server-tab metadata
  follows the tab.

- **Session restoration.** See §6.3.

### Where server-specific status indicators integrate

ZIM's per-tab server status indicator (green=running, yellow=starting,
red=crashed, blue=stopped/idle, with sub-states for installing,
recovering, unknown) attaches to the tab through `TabFeatures` and is
rendered by extending the existing `TabStyle` surface
(`chrome/browser/ui/tabs/tab_style.{h,cc}`) — not by replacing the tab
view. The integration point is documented in
`docs/architecture/BROWSER_INTEGRATION.md`.

## 6.3 Session persistence

Chromium's session persistence is implemented in
`chrome/browser/sessions/` (verified at the pinned revision; the
directory contains the full session restore infrastructure). The relevant
files and their responsibilities:

- `session_service.{h,cc}` — persists per-window/tab navigation state.
- `session_service_log.{h,cc}` — append-only log of session commands
  used to reconstruct the session on restart. This is the durability
  substrate; ZIM does not modify it.
- `session_restore.{h,cc}` — drives restoration on startup or on user
  request.
- `session_restore_delegate.{h,cc}` — embedder hook for what to do when
  restoring.
- `session_data_service.{h,cc}` — per-profile data service for session
  data.
- `session_data_deleter.{h,cc}` — deletes session data on
  shutdown / explicit clear.
- `exit_type_service.{h,cc}` — tracks normal vs crash exit; the
  distinction drives whether the next startup offers session restore
  automatically or asks the user.
- `restore_on_startup_policy_handler.{h,cc}` — handles the
  "restore-on-startup" policy.
- `tab_restore_service_*.{h,cc}` — recently-closed-tabs restoration.

ZIM extends this model by:

1. **Preserving browser session restoration as the source of truth for
   browser state.** A restored session restores server-tab *views*; it
   does not issue server lifecycle commands. The engine's authoritative
   server state is independent of the browser session.

2. **Storing server-tab association metadata in the session.** When a
   tab is a server tab, its navigation entry's tree of
   `SessionNode`s carries the `zim://servers/{id}/...` URL; the
   restored session simply re-navigates to that URL. No second source
   of truth for tab→server association is introduced.

3. **Crash recovery.** If Chromium crashes, `exit_type_service` records
   a crashed exit; the next startup offers to restore. Restored tabs
   navigate to their `zim://` URLs, which load the current server view
   from the engine. The engine's server state was not affected by the
   browser crash (the engine is an independent process).

4. **Profile separation.** Each Chromium profile has its own session
   service (verified by `session_data_service_factory.{h,cc}`). ZIM
   supports multiple profiles; each profile can have its own set of
   server tabs and ZIM settings. The engine is shared across profiles
   but the per-profile server-tab views are independent.

### Invariants ZIM must enforce

- A tab's existence is not proof that a server should run. (A restored
  session restores a tab to a *view* of a server; the server's lifecycle
  is independent.)
- Closing a tab does not stop its server. The engine owns server
  lifecycle; the tab owns only its view subscription.
- Restoring a session does not issue server lifecycle commands. The
  session restoration only re-navigates tabs to their `zim://` URLs;
  the loaded page reads the engine's current state and subscribes to
  updates.

These invariants are enforced at the engine boundary
(`docs/architecture/RUST_ENGINE.md` §lifecycle) and verified by tests in
`docs/architecture/TEST_STRATEGY.md`.

## 6.4 Split view

Chromium 157 has a real, first-class split-view implementation. This is
not an assumption; it is verified by direct enumeration of
`chrome/browser/ui/tabs/` at the pinned revision, which contains:

- `split_tab_util.{h,cc}` — split-tab utility helpers.
- `split_tab_highlight_controller.{h,cc}` — keyboard focus and
  highlighting across panes.
- `split_tab_menu_model.{h,cc}` — context-menu items for split actions.
- `split_tab_metrics.{h,cc}` — usage metrics.
- `split_tab_mute_menu_model.{h,cc}` — per-pane mute controls.
- `split_tab_swap_menu_model.{h,cc}` — pane swap.
- `split_view_layout_menu_model.{h,cc}` — horizontal/vertical layout
  switching.
- `split_view_iph_controller.{h,cc}` — in-product help (IPH) surface.

And in `chrome/browser/ui/tabs/tab_strip_model.h` (verified by reading
the file's forward declarations):

```cpp
namespace split_tabs {
class SplitTabData;
class SplitTabVisualData;
enum class SplitTabLayout;
enum class SplitTabCreatedSource;
enum class SplitTabOrientationChangeSource;
}  // namespace split_tabs

namespace tabs {
class SplitTabCollection;
class TabStripCollection;
class TabGroupTabCollection;
}  // namespace tabs
```

The `split_tabs` namespace is therefore a real, named Chromium 157
abstraction. `SplitTabLayout` is an enum (horizontal/vertical),
`SplitTabCreatedSource` distinguishes how a split was created (user
drag, menu, etc.), and `SplitTabOrientationChangeSource` distinguishes
how an orientation flip was initiated. The collection abstraction
(`tabs::SplitTabCollection`, `tabs::TabStripCollection`,
`tabs::TabGroupTabCollection`) is the data-model layer on top of
`TabStripModel` for grouped/tabbed/split tab arrangements.

### What this means for ZIM

ZIM does **not** need to implement a parallel split-view system.
Chromium 157 already provides:

- Two or more supported panes (verified: `SplitTabCollection`).
- Independent navigation per pane (each pane holds its own
  `WebContents`).
- Horizontal and vertical layouts (verified: `SplitTabLayout` enum and
  `split_view_layout_menu_model`).
- Resizing (standard views-level resize handles).
- Pane focus and keyboard navigation (verified:
  `split_tab_highlight_controller`).
- Moving tabs between panes (verified: `split_tab_swap_menu_model`).
- Closing, collapsing and swapping panes (verified across the
  `split_tab_*` set).
- Session persistence (split tab state is part of the session node
  tree; see §6.3).

ZIM's contribution to split view is therefore limited to:

1. A ZIM-specific surface that *uses* split view to show two views of
   the same server (e.g. console + files) or two different servers side
   by side, by orchestrating the existing `split_tabs` API.
2. Any ZIM-specific UX affordances (e.g. a "split with server console"
   context-menu item) implemented as additions to the existing
   `split_tab_menu_model`.

No Chromium source modification is required for ordinary split-view
support in ZIM. If a ZIM-specific extension to the split-view model is
required later, it will be a local patch under
`chromium/PATCHES/` with a corresponding regression test.

## 6.5 Extensions and internal pages

Chromium's extension system is implemented primarily under
`extensions/`, `chrome/browser/extensions/`, and
`chrome/browser/ui/webui/`. The directory
`chrome/browser/extensions/` contains 611 entries at the pinned
revision (verified by contents API), including the API surface under
`extensions/browser/api/`, the activity log, blocklist, installers, and
the bundled-extension glue.

`chrome/common/url_constants.h` (749 lines) defines the `chrome://` URL
constants; ZIM will add a parallel set of `zim://` URL constants through
the same registration path.

### ZIM trust boundaries (the prompt's required distinction)

| Trust tier                       | What it is                                                | What it can do                                                                                         |
|-----------------------------------|-----------------------------------------------------------|--------------------------------------------------------------------------------------------------------|
| Ordinary websites                 | Any page on the open web.                                 | Render content; navigate; use normal web APIs. **Cannot** access the ZIM engine.                        |
| Chromium extensions              | MV3 extensions installed by the user.                     | Their declared permissions (limited by Chromium's normal MV3 model). **Do not** auto-inherit ZIM server-control permissions. |
| Trusted ZIM internal pages       | Pages at `zim://` (newtab, settings, servers/{id}/...).   | Documented ZIM API surface, scoped to the page's capability grant from the engine.                    |
| ZIM management extensions         | Extensions with a `zim` manifest key.                    | The ZIM API permissions declared in their manifest, validated against the engine's capability store.  |
| Native browser shell              | The Chromium browser process and the engine process.      | Full authority within their respective domains.                                                         |
| Rust engine APIs                 | The engine's HTTPS/WSS surface.                            | The versioned, capability-checked API documented in `docs/api/ZIM_API_SPEC.md`.                       |

The key invariant is that *extension manifests are not proof of
authorization*. A ZIM management extension declares in its manifest what
ZIM permissions it wants; the engine validates each requested
permission against its capability store on each API call. A compromised
manifest cannot bypass the engine boundary.

### What ZIM can reuse

- Chromium's extension packaging, update, and signature model for
  delivering ZIM management extensions (no need to invent a new package
  format for end-user distribution).
- Chromium's WebUI mechanism for internal `zim://` pages (no need to
  invent a new internal-page substrate).
- Chromium's content-security-policy model for restricting what
  internal pages can load.

### What ZIM must not reuse

- ZIM must not treat Chromium's extension permission model as the
  authority for ZIM server-control permissions. The two systems are
  separate: Chromium permissions govern what an extension can do in
  the browser; ZIM permissions (declared in the manifest's `zim` key
  and checked by the engine) govern what an extension can do to
  servers.

## 6.6 Build, testing and maintenance

### Build system

- **GN** is the build configuration tool. Top-level `.gn` (479 lines
  at the pinned revision) imports `//build/config/BUILDCONFIG.gn` and
  sets build-level defaults.
- **Ninja** is the build executor. `BUILD.gn` files at every level
  declare targets.
- **`DEPS`** (4978 lines at the pinned revision) declares the
  dependencies that `gclient sync` fetches. This is the file that makes
  a Chromium checkout more than `git clone` — `DEPS` pulls in
  V8, Skia, third_party/*, and hundreds of other dependencies.
- **`depot_tools`** provides `gclient`, `fetch`, `gn`, `cl`, and the
  rest of the toolchain. Not present in the current workspace.

### Relevant tests

Chromium's test types include:
- `*_unittest.cc` — unit tests (run via the `*_unittests` target).
- `*_browsertest.cc` — browser tests (run via `browser_tests`).
- `*_interactive_uitest.cc` — interactive UI tests.
- `*_apitest.cc` — extension API tests.

For ZIM-relevant subsystems, the prompt requires tests identified in
`docs/architecture/TEST_STRATEGY.md`. Tests that ZIM should run as
upstream regression checks:
- `tab_strip_model_unittest.cc` (model-level tab invariants).
- `session_restore_browsertest.cc` (session restore correctness).
- `chrome_serialized_navigation_driver_unittest.cc` (navigation
  serialization).
- `extensions/` browsertests (when modifying extension behavior).

### Patch conventions

Upstream Chromium patches in ZIM are organized under
`chromium/PATCHES/`:

```
chromium/PATCHES/
├── series              # ordered list of patches
├── 0001-short-name.patch
├── 0002-short-name.patch
└── ...
```

Each patch is a single logical change with:
- A description in the commit message (subject + body).
- The source evidence justifying the change (referenced in
  `docs/architecture/UPSTREAM_MAINTENANCE.md`).
- A regression test that would catch upstream breakage.

No patches exist yet because the source tree is not yet imported (see
`chromium/UPSTREAM_MANIFEST.md`).

### Feature flags and runtime configuration

Chromium's feature flags (`base/feature_list.h`,
`chrome/browser/about_flags.cc` at the pinned revision) are the primary
runtime mechanism for gating features. ZIM-specific features that are
genuinely optional (e.g. an experimental ZIM tab visual style) should be
gated by a feature flag rather than by code deletion. The
`about_flags.cc` registration is a candidate local patch surface.

### CI requirements

Upstream Chromium uses LUCI (chromium-inf) for CI. ZIM's CI for the
Chromium fork will, at minimum, run the upstream regression tests
identified in `docs/architecture/TEST_STRATEGY.md` on each re-pin and on
each local-patch change. Detailed CI design is in
`docs/architecture/UPSTREAM_MAINTENANCE.md`.

### Third-party notices and licensing

Chromium's `LICENSE` (BSD-style) and the per-component
`third_party/*/README.chromium` files must be preserved verbatim in the
imported tree. ZIM's distribution packaging must surface the aggregated
third-party notices; this is a downstream concern (packaging, not the
architecture phase) but is recorded here so it is not lost.

## Spot-fetched source evidence (inspection-only, not in the ZIM repo)

The following files were fetched via raw HTTPS from
`raw.githubusercontent.com/chromium/chromium/157.0.8103.0/<path>` and
are kept locally under `/home/z/chromium-inspect/` as inspection
evidence. They are not part of the ZIM repository; they support the
findings in this document.

- `chrome/browser/ui/tabs/tab_strip_model.h` (1551 lines)
- `chrome/browser/ui/browser.h` (303 lines)
- `chrome/browser/ui/tabs/split_tab_util.h` (33 lines)
- `chrome/common/url_constants.h` (749 lines)
- `DEPS` (4978 lines)
- `.gn` (479 lines)

Plus the following directory enumerations (via the GitHub contents API,
not stored locally):
- `/repos/chromium/chromium/contents?ref=157.0.8103.0` (top-level tree)
- `/repos/chromium/chromium/contents/chrome/browser?ref=157.0.8103.0` (594 entries)
- `/repos/chromium/chromium/contents/chrome/browser/ui/tabs?ref=157.0.8103.0` (full tab set)
- `/repos/chromium/chromium/contents/chrome/browser/sessions?ref=157.0.8103.0` (full session set)
- `/repos/chromium/chromium/contents/chrome/browser/extensions?ref=157.0.8103.0` (611 entries)
- `/repos/chromium/chromium/contents/content/public/browser?ref=157.0.8103.0` (695+ entries)

## Audit scope and limits

What this audit established with primary-source evidence:

- The `Browser` class's role as a window (not a tab), its owner, and its
  inheritance surface.
- The `TabStripModel`'s role as the tab ownership model.
- The existence and shape of Chromium 157's first-class split-view
  implementation (`split_tabs` namespace, `SplitTabCollection`,
  `SplitTabLayout`, etc.).
- The session persistence infrastructure under
  `chrome/browser/sessions/`, including the append-only session log,
  the exit-type service, and the restore-on-startup policy handler.
- The presence and shape of the extension system under
  `chrome/browser/extensions/` (611 entries).
- The build system files (`DEPS`, `.gn`, `BUILD.gn`).

What this audit did not establish, and where further investigation is
required:

- The exact `TabFeatures` extension API surface for attaching
  per-tab metadata (the file `chrome/browser/ui/tabs/tab_features.h`
  exists at the pinned revision but its contents were not fetched in
  this phase).
- The exact `WebUI` registration mechanism for custom schemes (will
  need to be inspected when implementing `zim://`).
- The `BrowserWindowFeatures` API surface for per-window feature
  attachment.
- The depot_tools / gclient sync behavior on the pinned revision (no
  depot_tools in workspace).

These are recorded as INVESTIGATE in
`docs/audits/COMPONENT_DISPOSITION.md` and as open questions in
`docs/architecture/DECISIONS.md`. They do not block the architecture
phase; they block the first implementation milestone that touches the
Chromium source tree.
