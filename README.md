# ZIM — Zamin Instances Manager

ZIM is an independent desktop product of the ZaminMC ecosystem: a
Chrome-inspired desktop application powered by a Rust-based
workload-management engine. ZIM lets the user manage long-running
workloads — game servers, databases, websites, development processes,
generic containers, and future workload types introduced by
first-party modules or third-party extensions — through a
Chrome-style tabbed interface, while keeping the application and the
management engine as separate, independently-recoverable
authorities.

Minecraft and Pterodactyl are the original workload and reference
implementation that ZIM was bootstrapped from; the engine is generalized
beyond them through a typed workload abstraction with capability traits
(see `docs/architecture/WORKLOAD_ABSTRACTION.md`).

This repository is the engineering foundation for ZIM. It contains the real
upstream source trees of Pterodactyl Panel and Pterodactyl Wings (imported via
subtree merge with full upstream history preserved), the cancelled Chromium
import manifest (kept as provenance; see AD-20), the source-backed audits of
the Pterodactyl codebases and of Chrome's UX platform, the architecture and
API contracts for the Rust engine, the desktop frontend, and workload
abstraction, and the roadmap that subsequent implementation work will follow.

## What this repository is and is not

**It is:**

- A real, source-backed project: Pterodactyl Panel and Wings are imported
  as audited reference trees; Chrome's UX is the design reference for the
  frontend (no Chromium source — see AD-20).
- A working ZIM monorepo: Pterodactyl Panel and Wings source trees are present
  as `upstream/pterodactyl-panel/` and `upstream/pterodactyl-wings/` with their
  complete upstream history reachable through `git log`.
- The authoritative engineering specification for ZIM's Rust engine, browser
  integration, frontend replacement model, extension SDK, security boundaries
  and lifecycle invariants.
- A reproducible upstream pin: every imported project has a recorded revision,
  retrieval method, license and verification status in `docs/audits/`.

**It is not:**

- Dependent on Chromium source. The Chromium import is CANCELLED
  (AD-20, 2026-10-15): ZIM is a desktop application with a
  Chrome-inspired UI, not a browser. The desktop shell renders via the
  OS webview (WebView2 / WKWebView / WebKitGTK) — no Chromium source,
  no patch queue, no 100 GB build, ever. `chromium/UPSTREAM_MANIFEST.md`
  is kept as provenance for the cancelled decision; the pinned-revision
  audit (`docs/audits/CHROMIUM_ARCHITECTURE.md`) is reframed as a
  Chrome platform/UX reference for the frontend.
- A working ZIM runtime — partially. The Rust engine workspace
  (`zim/engine/`) is under active implementation following
  `docs/architecture/IMPLEMENTATION_ROADMAP.md`: the domain model +
  SQLite persistence (M3), authentication + authorization (M4), the
  HTTPS loopback API surface with the runnable `zim_engine` binary
  (M5), the runtime abstraction + Docker runtime (M6 — instance
  lifecycle with power-queue serialization, crash-loop protection,
  engine-restart reconciliation, and Wings-parity container
  management over `pterodactyl0`), streaming (M7 — the
  `zim_engine_streaming` fan-out hub with per-instance event
  generations, replay-from-checkpoint, drop-oldest backpressure,
  the `/api/v1/stream` WebSocket endpoint with per-tab capability
  token auth, supervision-published `instance.state_change` and
  `instance.resource_usage` events, and the console/metrics read
  endpoints), the full durable-jobs milestone (M8 — the
  `zim_engine_jobs` executor, the `zim_engine_backups`
  create/restore pipeline, the `zim_engine_transfers`
  two-phase-commit node move, and the `zim_engine_installs`
  constrained-container install pipeline over the `InstallRunner`
  port), the egg importer and per-instance cron scheduler (M9 —
  `zim_engine_eggs` and `zim_engine_scheduler`), the Pterodactyl
  Panel migration importer (M10 —
  `zim_engine_migrate_pterodactyl`), the frontend loader +
  default frontend (M11 — `zim_engine_frontend_loader` and the
  vanilla-TS/Web-Components package under `zim/frontend/`), the
  workload integration SDK + the five first-party integration
  crates (M16), the Minecraft integration at egg parity (M17 —
  the real file API, the egg-derived GameServerSpec, the console
  convention, the fakemc parity suite), the Database integration
  (M18 — Postgres / MariaDB / MySQL / Redis: the engine table,
  the readiness probes, the quiesced snapshot/restore family,
  the multi-step migration plan, and the real-container parity
  suite), and the remaining three first-party integrations (M19
  — the Website kind's static nginx carrier with the
  release-directory deploy family and the HTTP/TCP health
  checks, the DevProcess kind's `notify`-backed file-watch
  restart supervision, and the GenericContainer kind's
  placement-time capability negotiation), and the desktop shell
  scaffold (M21 — `zim/shell/`, a Tauri 2 application over the
  OS webview: engine discovery/attach via the data-dir
  `engine.json` file, detached engine spawn with lifecycle
  independence, `zim://` loopback-origin mapping (AD-23), OS
  trust-store installation of the engine's persistent loopback
  certificate, tray, and a 3-platform Shell CI workflow with a
  real-engine integration job), and the application chrome (M22 —
  `zim://app`: the Chrome-style tab strip and omnibox in the
  frontend's own TypeScript, same-origin iframe panes per AD-24,
  live §4 status indicators over the event stream, the §5 tab
  context menu, view-only session persistence, and a frontend
  unit-test layer in CI), and the Chrome-UX parity II batch (M23 —
  the §8 split view with independent per-pane streams and snapping
  divider, real multi-window: shell-owned tear-off/merge with
  per-window session keys (AD-26), the live theme system (presets +
  accent pip, AD-27) with the token vocabulary contract
  `zim/contracts/theme-tokens.v1.json`, and the completed new-tab
  page (search, engine-backed recents `/me/recents` (AD-28),
  create/import flows, workload browsing, customization)), and the
  extension / addon system (M24 — `zim_engine_extensions`: the
  `zim.addon.json` manifest + `.zimpkg` loader with the shared
  ed25519 trust root, the §10.12 lifecycle (install-as-approval,
  dependency resolution, surface-collision refusal, disable-only
  bundled defaults, audited moves), disable-with-notice error
  isolation (AD-29), `/zim/` addon surface + `zim://ext/{id}/`
  asset serving, the packaged-theme family `POST/GET/DELETE
  /api/v1/themes` against theme-tokens.v1, the documented addon
  SDK (`zim/sdk/addon/`, `docs/api/ADDON_SDK.md`) with the
  vendored `zim.js` client, the bundled
  `org.zaminmc.extension.audit-log` viewer + the `hello-zim`
  sample, and the frontend's `zim://extensions` management page
  with the chrome's §4.5-§4.7 contribution points), and the E2E
  suite + packaging milestone (M25 — `zim/e2e/`: a Node/Playwright
  runner that spawns the real `zim_engine` binary and drives the
  real product UI in a real browser across a 3-OS CI matrix, with
  AD-30's two-tier runtime model (real Docker lifecycle on Linux,
  honest degraded-mode skips elsewhere) and the audit-log-arbiter
  tab invariants; the update channel (AD-31,
  `docs/architecture/UPDATES.md`): GitHub Releases + minisign with
  the key in Actions secrets, the consent-shaped tray "Check for
  updates…" action, bundle-granular payloads; and the release
  pipeline `app-release.yml`: 3-platform installers with the
  engine as the `externalBin` sidecar, `.sig` updater artifacts,
  `latest.json` assembly, and the Linux packaged-app deb smoke
  under Xvfb), and the release milestones (M26 — the first
  alpha: tag `v0.1.0-alpha.1`, the honest what-works notes, the
  operator's Pterodactyl migration guide, the alpha labeling, and
  the engine-only `engine-v*` tag namespace so the App Release
  pipeline owns every plain `v*` tag; M27 — the release
  follow-ups that hardened the channel into `v0.1.0-alpha.2`:
  the first-boot spawn defect the packaged-app smoke caught
  (alpha.1's install bytes were dead-on-arrival — fixed with the
  data-dir creation + a regression test over the real attach),
  the `zim` to `zamin-zim` package rename that closed the archive
  collision with the distro `zim` wiki package, the engine
  control plane (a per-boot `engine.token` gating
  `POST /api/v1/engine/control`) with the coordinated Windows
  update shutdown and the tray's "Stop engine — servers keep
  running" (AD-32), the nightly failure-sweep workflow
  (`failure-sweep.yml`: E7-E9 visual mappings, the interrupted
  backup, disk-full, the live-MySQL migration E2E — a red sweep
  files an issue), AD-29's per-addon attribution hardening (the
  SDK stamps `X-ZIM-Addon`; the engine believes installed+enabled
  ids only; spoofed ids drop), the `zim://about` surface with the
  running-vs-installed engine version comparison, and the
  completed five-platform release matrix — Linux x64/arm64,
  Windows x64, macOS x64/arm64 — with per-arch deb smokes and a
  `latest.json` carrying all five platforms), and the file
  surface (M28 — the alpha's #1 published gap, closed as AD-33's
  one-tree-three-doorways: the real HTTP file API behind the seven
  declared §10.3 routes plus rename, over the same chrooted
  `Filesystem` the backup pipeline archives through, with the §13
  5 GB upload channel and `instance.file.*` audit rows; the
  `zim_engine_sftp` transport — a russh/ring SSH server whose
  SFTP subsystem serves the same tree with the same `file.*`
  grants through a synthetic per-instance root, password auth
  against the engine's own users with per-username throttling,
  an Ed25519 host key with the loopback certificate's lifecycle,
  and `127.0.0.1:2022` (Wings's own port) by default; and the
  `zim://servers/{id}/files` file manager — breadcrumb,
  listing, upload/download, new file/folder, rename, delete,
  archive/extract — with the E2E scenario rewritten from its
  honest 501 pin into the real three-OS suite plus a
  browser-driven UI walk and the SSH banner probe).
  M25's own acceptance campaign (16 CI batches) also earned a
  real engine fix: the production TLS accept loop was dropping
  every WebSocket subscriber on arrival (`serve_connection` had
  no upgrade support — the live event stream had silently never
  worked over the real server); fixed with
  `serve_connection_with_upgrades` and kept found by the
  `stream_tls_ws.rs` regression tests over the production loop.
  The release notes live at `docs/releases/ALPHA_v0.1.0.md` (the
  alpha.1 kit) and `docs/releases/ALPHA_v0.1.0-alpha.2.md`
  (alpha.2); the operator's migration walkthrough at
  `docs/compatibility/PTERODACTYL_MIGRATION_GUIDE.md`.
- A re-skinned Pterodactyl. The Panel's PHP/Laravel backend and Wings's Go
  daemon are preserved in `upstream/` as audit references and behavioral
  contracts; they are not part of ZIM's runtime architecture.

## Repository layout

```text
ZIM/
├── chromium/                    # Cancelled Chromium import manifest (provenance; AD-20)
│   └── UPSTREAM_MANIFEST.md      # Pinned revision, import strategy, status
├── upstream/
│   ├── pterodactyl-panel/        # Pterodactyl Panel v1.15.1 (subtree-merged)
│   └── pterodactyl-wings/        # Pterodactyl Wings v1.13.3 (subtree-merged)
├── zim/                          # ZIM's own code
│   ├── contracts/                # Versioned contract files
│   │   └── theme-tokens.v1.json  # Theme token vocabulary (AD-27)
│   ├── engine/                   # Rust engine workspace (31 crates,
│   │   │                         #   M3-M19; M27: the engine control
│   │   │                         #   plane + health's version; M28: the
│   │   │                         #   SFTP transport crate)
│   ├── shell/                    # Tauri 2 desktop shell (M21): window, tray,
│   │   │                         #   engine attach/spawn, zim:// mapping, trust;
│   │   │                         #   (M23: multi-window — tear-off / merge /
│   │   │                         #   new-window commands, window registry;
│   │   │                         #   M27: coordinated engine stop, updater)
│   │   ├── ui/                   #   bundled pre-engine pages (boot / recovery)
│   │   ├── icons/                #   app icons (png / ico / icns)
│   │   └── src/                  #   cli / zim_url / engine / trust / tray /
│   │                             #   state / windows / commands / control /
│   │                             #   updater
│   ├── frontend/                 # Default frontend package (engine-served)
│   │   │                         #   (M22: the zim://app chrome — tab strip +
│   │   │                         #   omnibox + panes; M23: split view, session
│   │   │                         #   v2, themes, the completed NTP with create /
│   │   │                         #   import / recents; M27: zim://about;
│   │   │                         #   M28: the zim://servers/{id}/files file
│   │   │                         #   manager; newtab, servers, console, and
│   │   │                         #   the declared per-instance surfaces;
│   │   │                         #   src/ TypeScript, tests/ node:test suite)
│   ├── e2e/                      # Node/Playwright E2E suite (M25): the real
│   │                             #   engine binary + real browser; the
│   │                             #   failure-scenario tests (M27: disk-full,
│   │                             #   interrupted backup, E7-E9 visual
│   │                             #   mappings, live-MySQL migration)
│   ├── sdk/                      # Addon SDK (M24): src/ + the vendored
│   │   └── addon/                #   zim.js client (M27: attribution stamp)
│   └── extensions/               # Bundled + sample addons (M24): the
│                                 #   audit-log viewer, hello-zim
├── docs/
│   ├── audits/                   # Upstream audits, baseline, dispositions
│   ├── architecture/             # ZIM architecture, Rust engine, desktop frontend
│   ├── api/                      # API + frontend/extension specs
│   ├── compatibility/            # Pterodactyl migration/compat
│   ├── releases/                 # Alpha release notes (v0.1.0-alpha.1/2)
│   ├── security/                 # Threat model
│   └── decisions/               # ADRs
├── LICENSE                       # Apache 2.0
└── ZIM-ICON-SOURCE.png           # ZIM brand icon source
```

## Building and running the desktop app (M21+)

The engine and the shell are separate cargo workspaces (the engine
stays free of GUI dependencies), and the UI the shell loads is the
engine-served frontend package (`zim/frontend/` — its compiled
output is committed because the engine embeds it at build time).
Build and run in a dev layout:

```bash
# 1. The engine (its own workspace):
cd zim/engine && cargo build

# 2. The shell, pointed at that engine binary:
cd ../shell && ZIM_ENGINE_PATH=../engine/target/debug/zim_engine cargo run
```

The shell attaches to an engine already serving the same data dir,
or spawns a detached one (its default data dir is the per-OS
app-data dir + `engine/`; override with `--data-dir`). Closing the
shell never stops the engine. The shell's window opens the
application chrome — `zim://app`: a tab strip and an omnibox
hosting the engine's pages as panes; the same chrome (and every
surface beneath it) also works in an ordinary browser at
`https://127.0.0.1:{engine-port}/zim/app`. Linux prerequisites are
the Tauri 2 system stack (`libwebkit2gtk-4.1-dev`, `libgtk-3-dev`,
`libayatana-appindicator3-dev`, `librsvg2-dev`, `libxdo-dev`); the
`Shell CI` workflow installs the exact set it builds with.

## Upstream pins

| Project              | Upstream URL                                          | Revision   | Retrieval date | Import method | License |
|----------------------|-------------------------------------------------------|------------|----------------|---------------|---------|
| Pterodactyl Panel    | https://github.com/pterodactyl/panel                  | v1.15.1    | 2026-10-13     | git subtree   | MIT     |
| Pterodactyl Wings    | https://github.com/pterodactyl/wings                  | v1.13.3    | 2026-10-13     | git subtree   | MIT     |
| Chromium             | https://chromium.googlesource.com/chromium/src.git   | 157.0.8103.0 | 2026-10-13   | cancelled (AD-20; UX reference only) | BSD-style |

Full traceable commit hashes and verification status are recorded in
`docs/audits/UPSTREAM_AUDIT.md`.

## How to read this repository

Start with:

1. `docs/audits/BASELINE.md` — Phase 0 workspace baseline.
2. `docs/audits/UPSTREAM_AUDIT.md` — what is imported, how, and what is verified.
3. `docs/audits/CHROMIUM_ARCHITECTURE.md` — Chrome platform/UX reference (integration cancelled; see AD-20).
4. `docs/audits/PTERODACTYL_ARCHITECTURE.md` — what Panel and Wings actually do.
5. `docs/audits/COMPONENT_DISPOSITION.md` — what ZIM keeps, adapts, replaces, removes, defers, or investigates.
6. `docs/architecture/ZIM_ARCHITECTURE.md` — the authoritative ZIM architecture.
7. `docs/architecture/IMPLEMENTATION_ROADMAP.md` — what happens next.

## License

ZIM's own code is Apache 2.0 (see `LICENSE`). Upstream source trees in
`upstream/` retain their original licenses (MIT for Pterodactyl, BSD-style for
Chromium when imported); upstream license files are preserved at the root of
each imported subtree.

## Status

ZIM has shipped its first alphas: `v0.1.0-alpha.1` and
`v0.1.0-alpha.2` (five desktop platforms each, installers + signed
updater payloads, published through the GitHub Releases update
channel). The repository remains the *engineering foundation* —
the architecture, contracts, audit evidence and roadmap — but it
is now also the shipping source: every release tag drives the
`App Release` pipeline and lands real installers. The product is
an **alpha**: not production-ready, honest about its boundaries
(see the release notes' what-works / what-doesn't sections), with
the nightly failure-sweep watching the honest-edge scenarios.
See `docs/architecture/IMPLEMENTATION_ROADMAP.md` for the
milestone history (M1-M27) and the post-alpha backlog.
