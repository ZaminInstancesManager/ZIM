# Feature Parity Matrix (Phase 4)

This matrix lists every upstream capability relevant to ZIM, the current
upstream implementation location, the intended ZIM behavior, the
replacement owner (which ZIM component is responsible), the
compatibility status, and the test evidence that will verify the
behavior.

It is the canonical cross-reference between the upstream audits
(`docs/audits/CHROMIUM_ARCHITECTURE.md`,
`docs/audits/PTERODACTYL_ARCHITECTURE.md`) and the ZIM architecture
(`docs/architecture/ZIM_ARCHITECTURE.md`,
`docs/architecture/RUST_ENGINE.md`,
`docs/architecture/BROWSER_INTEGRATION.md`). Where this matrix and
another document disagree, this matrix is canonical for *what* ZIM
does; the architecture documents are canonical for *how* ZIM does it.

Legend for compatibility status:

- **Compatible** — behavior preserved at the API contract level.
- **Behavior-compatible** — behavior preserved but the API differs.
- **Adapter** — preserved via an importer/adapter; minor differences
  documented in `docs/compatibility/MIGRATION_AND_COMPATIBILITY.md`.
- **Replaced** — behavior is implemented by a different component;
  upstream surface is gone.
- **Deferred** — not in the first ZIM release.
- **Removed** — behavior is gone and not replaced.

Legend for test evidence:

- **Planned** — the test is identified in
  `docs/architecture/TEST_STRATEGY.md` but not yet written.
- **Upstream-protected** — an upstream test that runs as regression
  coverage for this behavior.
- **Engine-unit** — Rust unit test of the engine behavior.
- **Engine-integration** — Rust integration test against a runtime.
- **Browser-test** — Chromium browser_tests target.
- **E2E** — end-to-end scenario test.

## 1. Browser / Chromium capabilities

| Upstream feature          | Upstream implementation                            | ZIM behavior                                       | Owner                                             | Compat status | Test evidence |
|---------------------------|----------------------------------------------------|----------------------------------------------------|---------------------------------------------------|---------------|----------------|
| Browser window            | `chrome/browser/ui/browser.h` `Browser` class     | Preserved as-is                                   | Chromium upstream                                 | Compatible    | Upstream-protected (`browser_tests`), browser-test |
| Tabs                      | `chrome/browser/ui/tabs/tab_strip_model.{h,cc}`   | Preserved as-is; ZIM attaches metadata via `TabFeatures` | Chromium upstream + `zim/integration/server_tab_feature` | Compatible | Upstream-protected (`tab_strip_model_unittest.cc`), engine-integration |
| Tab groups                | `tab_group_model.{h,cc}`                          | Preserved                                          | Chromium upstream                                 | Compatible    | Upstream-protected |
| Pinned tabs               | `pinned_tab_service.{h,cc}`, `pinned_tab_codec.{h,cc}` | Preserved                                          | Chromium upstream                                 | Compatible    | Upstream-protected (`pinned_tab_service_browsertest.cc`) |
| Recently closed           | `tab_restore_service_*.{h,cc}`                    | Preserved                                          | Chromium upstream                                 | Compatible    | Upstream-protected (`tab_restore_service_browsertest.cc`) |
| Session restore           | `session_restore.{h,cc}`, `session_service.{h,cc}` | Preserved; restoring a session does NOT issue server lifecycle commands | Chromium upstream + `zim_engine::supervision` (boundary invariant) | Compatible | Upstream-protected (`session_restore_browsertest.cc`) + engine-integration (invariant test) |
| Exit type tracking        | `exit_type_service.{h,cc}`                        | Preserved                                          | Chromium upstream                                 | Compatible    | Upstream-protected (`exit_type_service_browsertest.cc`) |
| Restore-on-startup policy | `restore_on_startup_policy_handler.{h,cc}`        | Preserved                                          | Chromium upstream                                 | Compatible    | Upstream-protected |
| Bookmarks                 | `chrome/browser/bookmarks/`, `chrome/browser/ui/bookmarks/` | Preserved; users can bookmark `zim://` URLs | Chromium upstream                                 | Compatible    | Upstream-protected |
| History                    | `chrome/browser/history/`, `components/history/`  | Preserved                                          | Chromium upstream                                 | Compatible    | Upstream-protected |
| Omnibox                    | `chrome/browser/ui/omnibox/`, `chrome/browser/ui/views/omnibox/` | Preserved (unmodified)                          | Chromium upstream                                 | Compatible    | Upstream-protected |
| New-tab page              | `chrome/browser/ui/webui/ntp/`                    | ZIM replaces default NTP with `zim://newtab`       | `zim/frontend/` + WebUI registration              | Replaced      | E2E (ZIM NTP smoke) |
| Context menus             | `tab_menu_model.{h,cc}`                            | Preserved + ZIM contributes server-tab menu items via `existing_*_sub_menu_model` extension | Chromium upstream + `zim/integration/`            | Compatible    | Upstream-protected + engine-integration |
| Keyboard shortcuts        | `chrome/browser/ui/views/accelerators/`            | Preserved; ZIM-specific accelerators via `BrowserWindowFeatures` | Chromium upstream + `zim/integration/`            | Compatible    | Upstream-protected |
| Window management         | `chrome/browser/ui/browser.h`, `browser_window/`   | Preserved                                          | Chromium upstream                                 | Compatible    | Upstream-protected |
| Browser settings         | `chrome/browser/ui/webui/settings/`               | Preserved; ZIM settings at `zim://settings`        | Chromium upstream + `zim/frontend/`               | Compatible    | Upstream-protected + E2E |
| `chrome://` internal pages | `chrome/browser/ui/webui/`, `chrome/common/url_constants.h` | Preserved; `zim://` pages added via same mechanism | Chromium upstream + `zim/integration/webui_registration` (local Chromium patch) | Compatible | Upstream-protected + engine-integration |
| Page load indicators      | (various)                                          | Preserved                                          | Chromium upstream                                 | Compatible    | Upstream-protected |
| Drag and drop             | (various)                                          | Preserved; server-tab metadata follows the tab    | Chromium upstream + `zim/integration/`            | Compatible    | Upstream-protected |
| Split view                | `split_tabs` namespace, `split_tab_*`/`split_view_*` files | Preserved; ZIM uses the existing API for "two server views side-by-side" | Chromium upstream                                 | Compatible    | Upstream-protected |
| Extensions (MV3)          | `extensions/`, `chrome/browser/extensions/`        | Preserved                                          | Chromium upstream                                 | Compatible    | Upstream-protected (`extensions_apitests`) |
| Site isolation / sandbox  | `content/public/browser/`, `sandbox/`             | Preserved (ZIM does NOT weaken sandbox)            | Chromium upstream                                 | Compatible    | Upstream-protected |
| Network service          | `services/network/`, `net/`                       | Preserved                                          | Chromium upstream                                 | Compatible    | Upstream-protected |
| Renderer crash recovery   | (various)                                          | Preserved; engine state not affected by renderer crash | Chromium upstream                                 | Compatible    | Upstream-protected + engine-integration (invariant) |
| Multi-profile             | `chrome/browser/profiles/`                       | Preserved                                          | Chromium upstream                                 | Compatible    | Upstream-protected |
| Crashpad / diagnostics    | `components/crash/`                               | Preserved; ZIM-specific crash keys via local patch | Chromium upstream                                 | Compatible    | Upstream-protected |
| Auto-update              | `components/update_launcher/` (where present)     | Replaced with ZIM-specific updater (preserves Chromium update mechanism where possible) | `zim/updater/` (TBD)                              | Replaced      | E2E (update smoke) |

## 2. Server-management capabilities (replacing Pterodactyl)

| Upstream feature                       | Upstream implementation                                   | ZIM behavior                                                            | Owner                                              | Compat status | Test evidence |
|-----------------------------------------|-----------------------------------------------------------|-------------------------------------------------------------------------|----------------------------------------------------|---------------|----------------|
| User authentication                     | Panel `app/Services/Users/`, `app/Http/Controllers/Auth/` | Local user accounts + (optional) OAuth                                  | `zim_engine::auth`                                 | Behavior-compatible | Engine-unit, engine-integration |
| Per-subuser-per-server permissions      | Panel `app/Models/Permission.php`                        | Permission vocabulary preserved verbatim; engine checks at API boundary | `zim_engine::authz`                                | Compatible    | Engine-unit |
| Server creation                         | Panel `app/Services/Servers/ServerCreationService.php`   | Engine service reproduces the contract (allocation select, validate, persist, install) | `zim_engine::domain::servers`                     | Behavior-compatible | Engine-integration |
| Server deletion                         | Panel `app/Services/Servers/ServerDeletionService.php`   | Engine deletes instance, files, allocations                            | `zim_engine::domain::servers`                     | Behavior-compatible | Engine-integration |
| Server config + startup variables       | Panel `app/Services/Servers/` (various)                  | Engine reproduces validation + persistence                             | `zim_engine::domain::servers` + `::eggs`           | Behavior-compatible | Engine-integration |
| Node management                         | Panel `app/Services/Nodes/`                               | Engine reproduces + extends with remote-node contract                    | `zim_engine::domain::nodes`                       | Behavior-compatible | Engine-integration |
| Port allocations                        | Panel `app/Services/Allocations/`                        | Engine reproduces allocation assignment + atomicity                     | `zim_engine::domain::allocations`                 | Behavior-compatible | Engine-integration |
| Docker image selection                  | Panel `app/Models/Egg.php`                               | Engine preserves egg's image list                                       | `zim_engine::domain::eggs`                        | Compatible    | Engine-integration |
| Egg install scripts                    | Wings `server/install.go`                                | Engine runs install script in constrained container                     | `zim_engine::supervision::install`               | Behavior-compatible | Engine-integration |
| Egg update scripts                     | Wings `server/update.go`                                | Engine runs update script                                               | `zim_engine::supervision::install`               | Behavior-compatible | Engine-integration |
| Server lifecycle (start/stop/restart/kill) | Wings `server/power.go` (`PowerAction` enum)            | Engine preserves 4-action vocabulary + per-instance serialization       | `zim_engine::supervision::lifecycle`             | Compatible    | Engine-integration |
| Process supervision (Docker)           | Wings `environment/docker.go`                            | Engine `DockerRuntime` implements `Runtime` trait                       | `zim_engine::supervision::runtime::docker`        | Replaced      | Engine-integration |
| Process supervision (host)              | (not in upstream — ZIM extension)                       | ZIM `HostRuntime` for non-containerized execution                       | `zim_engine::supervision::runtime::host` (TBD)    | Replaced      | Engine-integration |
| Crash detection                         | Wings `server/crash.go` `CrashHandler`                   | Engine reproduces crash-detection gating + crash-loop backoff           | `zim_engine::supervision::crash`                 | Behavior-compatible | Engine-integration |
| Console streaming (output → subscriber) | Wings `server/console.go`, `server/websockets.go`       | Engine fan-out to per-tab subscribers                                   | `zim_engine::streaming::console`                 | Behavior-compatible | Engine-integration |
| Console input (subscriber → process)   | Wings `server/console.go`                                | Engine validates `control.console` permission then forwards             | `zim_engine::streaming::console`                 | Behavior-compatible | Engine-integration |
| WebSocket subscribers (multiple per server) | Wings `server/websockets.go` `WebsocketBag`            | Engine preserves multi-subscriber model (a tab is a subscriber)         | `zim_engine::streaming::subscriptions`           | Compatible    | Engine-integration |
| File operations (list/read/write/delete) | Wings `server/filesystem/`, `router/router_server_files.go` | Engine exposes file API; archive/extract with security-tight limits     | `zim_engine::files`                               | Behavior-compatible | Engine-integration |
| SFTP                                    | Wings `sftp/`                                             | Engine may expose SFTP for backwards compat                              | `zim_engine::files::sftp` (TBD)                   | Deferred      | Engine-integration |
| Backups (create/restore)                | Wings `server/backup/`, `router/router_server_backup.go` | Engine reproduces; backup is a durable job                              | `zim_engine::backups`                             | Behavior-compatible | Engine-integration |
| Transfers (move server between nodes)   | Wings `server/transfer/`, `router/router_server_transfer.go` | Engine reproduces; transfer is a durable job with explicit state machine | `zim_engine::transfers`                          | Behavior-compatible | Engine-integration |
| Schedules (per-server cron)             | Panel `app/Services/Schedules/`, Wings `gocron`           | Engine `Scheduler` module reproduces                                    | `zim_engine::scheduler`                           | Behavior-compatible | Engine-integration |
| Resource monitoring (CPU/mem/disk/io)   | Wings `environment/stats.go`, `ResourceEvent`            | Engine exposes metrics stream                                          | `zim_engine::metrics`                             | Behavior-compatible | Engine-integration |
| Activity / audit log                    | Panel `app/Models/ActivityLog.php`, Wings activity forwarding | Engine has own audit module; contract of what is logged preserved       | `zim_engine::audit`                               | Behavior-compatible | Engine-integration |
| API keys                                | Panel `app/Services/Api/`, `app/Models/ApiKey.php`       | Engine reproduces; bearer-token auth                                   | `zim_engine::auth::api_keys`                      | Behavior-compatible | Engine-integration |
| Client API surface                      | Panel `routes/api-client.php`, `app/Http/Controllers/Api/Client/` | Engine reproduces for legacy compat (deferred to a later milestone)     | `zim_engine::api::legacy_client` (TBD)            | Deferred      | Engine-integration |
| Application API surface                 | Panel `routes/api-application.php`                       | Engine reproduces for legacy compat (deferred)                          | `zim_engine::api::legacy_application` (TBD)      | Deferred      | Engine-integration |
| Remote (Panel↔Wings) API surface        | Panel `routes/api-remote.php`                            | Replaced by engine's internal node-runtime contract                     | `zim_engine::node::runtime`                      | Replaced      | Engine-integration |
| Databases (managed MySQL/Postgres)      | Panel `app/Services/Databases/`, `app/Models/Database.php` | Engine reproduces; deferred to later milestone                          | `zim_engine::domain::databases` (TBD)            | Deferred      | Engine-integration |
| Subusers                                | Panel `app/Services/Subusers/`, `app/Models/Permission.php` | Engine reproduces; `Permission` vocabulary preserved                    | `zim_engine::domain::subusers` + `::authz`        | Compatible    | Engine-integration |
| Eggs (server definitions)              | Panel `app/Services/Eggs/`, `app/Models/Egg.php`         | Engine preserves egg format via importer; native `zim.toml` preferred   | `zim_engine::eggs::importer` + `::definition`     | Adapter       | Engine-integration |
| Telemetry                               | Panel `app/Services/Telemetry/`                         | Engine reproduces; opt-in                                              | `zim_engine::telemetry` (TBD)                      | Behavior-compatible | Engine-integration |
| Mounts (bind mounts into containers)    | Panel `app/Models/Mount.php`, Wings `server/mounts.go`   | Engine reproduces                                                       | `zim_engine::supervision::mounts`                 | Behavior-compatible | Engine-integration |

## 3. ZIM-original capabilities (not in either upstream)

These are capabilities ZIM introduces that are not present in either
Chromium or Pterodactyl. They are listed here for completeness so the
parity matrix is honest about what is ZIM-original.

| ZIM feature                              | Description                                                            | Owner                                              | Test evidence |
|------------------------------------------|------------------------------------------------------------------------|----------------------------------------------------|---------------|
| `zim://` URL scheme                      | Internal navigation for ZIM pages                                      | `zim/integration/webui_registration` (Chromium patch) + `zim/frontend/` | E2E |
| Server-tab status indicator              | Per-tab visual indicator of server state (green/yellow/red/blue)       | `zim/integration/server_tab_feature`               | E2E |
| Server-tab context menu items            | "Open console", "Start server", "Stop server", etc.                   | `zim/integration/tab_menu`                         | E2E |
| Frontend replacement (themes/packages/full) | Multi-level customization                                            | `zim/frontend/` + `zim/extensions/`               | E2E |
| ZIM addon SDK                            | Extension manifest `zim` key + ZIM permissions checked at engine boundary | `zim/extensions/` + `zim_engine::extensions`     | Engine-integration + E2E |
| Default frontend restoration             | Recover from a broken custom frontend                                  | `zim/frontend/` + `zim_engine::frontend_loader`   | E2E (recovery) |
| Engine-isolated failure                  | Engine remains operational if renderer crashes or all browser windows close | `zim_engine::runtime`                              | Engine-integration |
| Remote-node contract                     | Authenticated, versioned contract for remote execution                 | `zim_engine::node::runtime`                        | Engine-integration |
| Migration importer                       | Read existing Pterodactyl MySQL DB → ZIM DB                            | `zim_engine::migrate::pterodactyl`                 | Engine-integration |

## 4. Verification discipline

The prompt requires that we never label an unexecuted test as passing. The
"Test evidence" column above identifies *which test will verify each
capability*; it does **not** assert that the test has passed. As of the
end of the architecture phase:

- **Upstream-protected** tests: not run in this workspace (no Chromium
  build, no PHP runtime, no Go toolchain). They will be run on the
  first build environment with sufficient capacity.
- **Engine-unit** tests: not run (no Rust toolchain). They will be
  written during the first implementation milestone.
- **Engine-integration** tests: not run (no Rust toolchain, no Docker
  runtime). They will be written during the first implementation
  milestone.
- **Browser-test** tests: not run (no Chromium build).
- **E2E** tests: not run (no ZIM runtime yet).

The matrix is therefore a *contract* for what tests must exist and
pass before any capability is claimed as "Verified". The TEST_STRATEGY
document (`docs/architecture/TEST_STRATEGY.md`) is the canonical
reference for the test inventory.
