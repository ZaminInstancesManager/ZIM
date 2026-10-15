# ZIM Engineering Foundation — Completion Report

> **CORRECTION (2026-10-15, AD-20):** This report describes the
> architecture phase as originally planned, including the
> (then-planned) Chromium-fork direction. The Chromium direction was
> superseded on 2026-10-15 — see AD-20/AD-21/AD-22 and the
> rewritten `docs/architecture/BROWSER_INTEGRATION.md` and
> `docs/architecture/IMPLEMENTATION_ROADMAP.md`. The Pterodactyl
> imports, audits, engine milestones M1-M19, and the frontend
> package described here remain valid and in place.

This report records the completion of the architecture-phase
assignment for ZIM (Zamin Instances Manager). It follows the format
specified in §17 of the assignment prompt.

**Date:** 2026-10-13 (UTC+08:00).
**Committer:** GitHub user `Abasing` (id 140362885).
**Repository:** https://github.com/ZaminInstancesManager/ZIM.git
**Branch:** `main`.

## 1. Repository changes

### 1.1 Source trees imported

| Source tree                  | Upstream URL                          | Pinned revision | Full commit SHA                                  | Import method                         | Source present in repo | Build-verified |
|-------------------------------|-----------------------------------------|------------------|----------------------------------------------------|----------------------------------------|------------------------|----------------|
| Pterodactyl Panel             | https://github.com/pterodactyl/panel    | `v1.15.1`        | `e98678c5a1d07a74036660a459d87a4eb7c105c1`        | `git subtree add` (history-preserving) | Yes                    | No             |
| Pterodactyl Wings             | https://github.com/pterodactyl/wings     | `v1.13.3`        | `6987d5e6f0612133e031255a99655e822d30579a`       | `git subtree add` (history-preserving) | Yes                    | No             |
| Chromium                      | https://chromium.googlesource.com/chromium/src.git | `157.0.8103.0` | `b0a05135bf5a60bb3ab5a1e86a0e55193cb126d0`       | Deferred (manifest-only at this phase) | No                     | No             |

### 1.2 Upstream revisions

See §1.1 above. The full upstream audit with verification status is
in `docs/audits/UPSTREAM_AUDIT.md`. The Chromium deferred-import
strategy is in `chromium/UPSTREAM_MANIFEST.md`.

### 1.3 Repository structure

```
ZIM/
├── LICENSE                       # Apache 2.0 (preserved from initial repo)
├── ZIM-ICON-SOURCE.png           # ZIM brand icon source (preserved)
├── README.md                     # ZIM README (authored this phase)
├── chromium/
│   ├── UPSTREAM_MANIFEST.md      # Pinned revision + deferred import strategy
│   └── PATCHES/                  # Local Chromium patch queue (empty initially)
├── upstream/
│   ├── pterodactyl-panel/        # Panel v1.15.1 (subtree-merged)
│   └── pterodactyl-wings/        # Wings v1.13.3 (subtree-merged)
├── zim/
│   ├── engine/                   # Rust engine workspace (skeleton; M2 milestone)
│   ├── contracts/                # Versioned IPC/API contracts (TBD)
│   ├── frontend/                 # Default frontend package (TBD)
│   ├── extensions/               # Bundled ZIM addons (TBD)
│   └── integration/              # Browser-side C++ integration layer (TBD)
├── docs/
│   ├── audits/
│   │   ├── BASELINE.md           # Phase 0 workspace baseline
│   │   ├── UPSTREAM_AUDIT.md     # Phase 1 upstream verification
│   │   ├── CHROMIUM_ARCHITECTURE.md # Phase 2 Chromium audit
│   │   ├── PTERODACTYL_ARCHITECTURE.md # Phase 3 Pterodactyl audit
│   │   ├── COMPONENT_DISPOSITION.md # Phase 4 KEEP/ADAPT/REPLACE/REMOVE/DEFER/INVESTIGATE
│   │   ├── FEATURE_PARITY_MATRIX.md # Phase 4 parity matrix
│   │   └── REMOVALS.md           # Phase 5 removals record
│   ├── architecture/
│   │   ├── ZIM_ARCHITECTURE.md  # Authoritative architecture
│   │   ├── RUST_ENGINE.md       # Rust engine workspace + domain + supervision
│   │   ├── BROWSER_INTEGRATION.md # Chromium integration points
│   │   ├── PLATFORM_SUPPORT.md  # Windows/Linux/macOS/remote-node matrix
│   │   ├── TEST_STRATEGY.md     # Test types + invariant inventory
│   │   ├── UPSTREAM_MAINTENANCE.md # Patch carrying + re-pin strategy
│   │   ├── IMPLEMENTATION_ROADMAP.md # 15-milestone roadmap
│   │   └── DECISIONS.md         # 12 ADRs + 12 unresolved questions
│   ├── api/
│   │   ├── ZIM_API_SPEC.md      # Versioned HTTPS+WSS API
│   │   └── FRONTEND_EXTENSION_SPEC.md # Themes + frontend packages + addon SDK
│   ├── compatibility/
│   │   └── MIGRATION_AND_COMPATIBILITY.md # Pterodactyl migration paths
│   └── security/
│       └── THREAT_MODEL.md      # 6 trust tiers + 16 attack scenarios
├── tools/                       # Repo scripts (maintenance scripts TBD)
└── tests/                       # Cross-component tests (TBD)
```

### 1.4 Git and working-tree impact

| Item                                  | State at start | State at end |
|----------------------------------------|-----------------|---------------|
| ZIM repo branches                       | `main` (1 commit) | `main` (8 commits) |
| ZIM repo commit count                   | 2 (initial + icon upload) | 2 + 6 new architecture-phase commits |
| Files in working tree (ZIM-original content) | 2 (LICENSE + icon) | 2 + 21 new docs + chromium manifest |
| Files in working tree (upstream Pterodactyl content, subtree-merged) | 0 | 1665 (combined Panel + Wings) |
| `git log --oneline` count              | 2 | 6265 (includes full Pterodactyl history via subtree merge) |
| Disk usage of `.git`                   | ~13 MB (icon) | ~30 MB |
| Disk usage of working tree              | ~1.4 MB (icon) | ~16 MB upstream + ~280 KB docs |
| Unrelated user work                     | Apache LICENSE + brand icon | Preserved untouched |

## 2. Audit coverage

### 2.1 Chromium subsystems inspected

Per `docs/audits/CHROMIUM_ARCHITECTURE.md`:

- Browser process architecture: `Browser` class
  (`chrome/browser/ui/browser.h`, 303 lines), `BrowserManagerService`
  ownership, `TabStripModel` (`chrome/browser/ui/tabs/tab_strip_model.h`,
  1551 lines), `content::WebContents`.
- Browser UI / navigation: full enumeration of
  `chrome/browser/ui/tabs/` (150+ entries); tab strip model, tab
  groups, pinned tabs, recently closed, omnibox, bookmarks, history
  presence confirmed.
- Session persistence: full enumeration of
  `chrome/browser/sessions/` (~70 files) including
  `session_service.{h,cc}`, `session_restore.{h,cc}`,
  `session_service_log.{h,cc}`, `exit_type_service.{h,cc}`,
  `restore_on_startup_policy_handler.{h,cc}`,
  `tab_restore_service_*.{h,cc}`.
- Split view: confirmed first-class in Chromium 157. `split_tabs`
  namespace forward-declared in `tab_strip_model.h` with five
  types (`SplitTabData`, `SplitTabVisualData`, `SplitTabLayout`,
  `SplitTabCreatedSource`, `SplitTabOrientationChangeSource`).
  Eight `split_tab_*`/`split_view_*` source files enumerated in
  `chrome/browser/ui/tabs/`.
- Extensions and internal pages: 611 entries enumerated in
  `chrome/browser/extensions/`. `chrome/common/url_constants.h`
  (749 lines) read for `chrome://` URL constants inventory.
- Build / testing / maintenance: `.gn` (479 lines), `DEPS` (4978
  lines), `BUILD.gn` top-level entries enumerated. `about_flags.cc`
  presence confirmed.

### 2.2 Panel and Wings subsystems inspected

Per `docs/audits/PTERODACTYL_ARCHITECTURE.md`:

**Panel:**
- 17 service subdirectories under `app/Services/` (Acl, Activity,
  Allocations, Api, Backups, Databases, Deployment, Eggs, Helpers,
  Locations, Nests, Nodes, Schedules, Servers, Subusers, Telemetry,
  Users) — direct mapping to Rust engine modules.
- `Server` Eloquent model (`app/Models/Server.php`) with full
  `@property` list (id, external_id, uuid, uuidShort, node_id,
  name, description, status, skip_scripts, owner_id, memory, swap,
  disk, io, cpu, threads, oom_disabled, allocation_id, nest_id,
  egg_id, startup, image, allocation_limit, database_limit,
  backup_limit, created_at, updated_at, installed_at).
- `Permission` constants vocabulary (`app/Models/Permission.php`)
  extracted: `websocket.connect`, `control.console`,
  `control.start`, `control.stop`, `control.restart`,
  `database.*`, `schedule.*`, `user.*`, `backup.read`.
- Panel→Wings RPC surface in `routes/api-remote.php` (11 endpoints
  documented): SFTP auth, server list, server details, install
  status, transfer status, backup status.
- 195 migrations enumerated (2016-01-23 to 2024-07-13).

**Wings:**
- `Server` struct (`server/server.go`): per-server mutex,
  per-server context + cancel, `powerLock *system.Locker`,
  `Environment environment.ProcessEnvironment`,
  `fs *filesystem.Filesystem`, `client remote.Client`.
- `PowerAction` enum + serialized-execution invariant (single-worker
  workerpool per server) in `server/power.go`.
- `ProcessEnvironment` interface + four-state model
  (`ProcessOfflineState`, `ProcessStartingState`,
  `ProcessRunningState`, `ProcessStoppingState`) in
  `environment/environment.go`.
- `CrashHandler` struct + `handleServerCrash` gating logic in
  `server/crash.go`.
- `Install()` + `install(reinstall)` Docker-containerized
  install behavior in `server/install.go`.
- `WebsocketBag` per-server multi-subscriber model in
  `server/websockets.go`.
- `Manager` struct + `NewManager`/`NewEmptyManager` in
  `server/manager.go`.
- `Docker()` singleton client + `ConfigureDocker` network setup in
  `environment/docker.go`.
- Router file inventory in `router/` (12 files + subdirectories).
- Go module dependencies in `go.mod` (Docker, gin, gorilla/websocket,
  glebarez/sqlite, gocron, jwt, etree, workerpool, etc.).

### 2.3 Important findings and confirmed architectural constraints

- Chromium 157 has a real, first-class split-view implementation
  (`split_tabs` namespace, `SplitTabCollection`, `SplitTabLayout`).
  ZIM does not need to implement a parallel split-view system; it
  uses the existing one.
- The Panel's `app/Services/` decomposition maps directly to the
  Rust engine's domain module structure (17 service subdirs → 17
  Rust engine modules in the proposed workspace).
- Wings's per-server power-action serialization (single-worker
  workerpool) is the canonical "per-instance operation
  serialization" invariant the prompt requires. ZIM's Rust engine
  preserves it.
- Wings's four-state lifecycle model matches the prompt's required
  four-primary-status model. ZIM preserves the four and adds
  `crash_loop` and `unknown` as extensions.
- The Panel's `Permission` constants vocabulary is preserved
  verbatim in ZIM's `PermissionGrant` enum for tool compatibility.
- The browser and engine must be separate OS processes (AD-4) to
  satisfy the prompt's "engine remains operational if Chromium
  crashes, a renderer exits, a frontend package fails to load, or
  a user closes every browser window" invariant.

### 2.4 Areas that remain unexamined

Per `docs/audits/COMPONENT_DISPOSITION.md` §5 (INVESTIGATE):

1. `TabFeatures` API surface (the file exists; full contents not
   fetched in this phase).
2. `BrowserWindowFeatures` API surface.
3. WebUI registration mechanism for custom schemes.
4. Full `ProcessEnvironment` interface method set beyond the head
   of `environment.go`.
5. Full `CrashHandler.handleServerCrash` body (crash-loop
   threshold/backoff logic).
6. Full `Server.install(reinstall)` body beyond the head.
7. Panel-side `app/Repositories/Wings/` content.

These do not block the architecture phase; they are blockers for
the corresponding implementation milestones (recorded in
`docs/architecture/IMPLEMENTATION_ROADMAP.md`).

## 3. Cleanup

### 3.1 Components removed or disabled

**Zero upstream source was removed or disabled in the architecture
phase.** This is the correct outcome of Phase 5 for this repository
at this phase, as documented in `docs/audits/REMOVALS.md`:

- No Chromium source is present (deferred per import strategy).
- Pterodactyl Panel and Wings subtrees are preserved as audit
  reference.
- The Panel's traditional dashboard frontend is not built by ZIM
  (source preserved in `upstream/` subtree; not used in ZIM's
  runtime).
- The Panel's deployment artifacts (Dockerfile, Nix flake) are not
  used in ZIM's runtime (source preserved).

### 3.2 Components deliberately retained

Per `docs/audits/COMPONENT_DISPOSITION.md`:

- All required Chromium browser infrastructure (browser process,
  tab model, session restore, split view, extensions, sandbox,
  network service, bookmarks, history, etc.) is marked **KEEP**.
- All Pterodactyl behavioral contracts (Permission vocabulary,
  four-state lifecycle, power-action vocabulary, egg format) are
  marked **KEEP** (as contracts; implementation is replaced in
  Rust).
- The Pterodactyl Panel and Wings source trees are preserved as
  audit reference (marked **REPLACE** for behavior; source kept as
  reference).

### 3.3 Components deferred because removal was not justified

Per `docs/audits/REMOVALS.md` §deferred:

- R1: Panel frontend source (after ZIM default frontend is
  feature-complete).
- R2: Panel deployment artifacts (after ZIM packaging is complete).
- R3: Wings daemon entry and router (after engine reaches parity).
- R4: Panel application code (after engine + migration importer
  reach parity).
- R5: Chromium bundled applications / demo pages (after Chromium
  source is imported; candidates TBD).

Each candidate has documented trigger conditions and validation
requirements so the implementation phase can execute the removal
safely.

### 3.4 Validation results and regressions

Per `docs/audits/REMOVALS.md` §validation:

- No unverified mass deletion: verified (zero upstream source
  deleted).
- Browser infrastructure required by ZIM remains intact: verified
  (no Chromium source to delete; disposition matrix marks
  required Chromium components KEEP).
- Server-management infrastructure required by ZIM remains intact
  (as audit reference): verified.
- All cleanup decisions recorded: verified (in
  `docs/audits/COMPONENT_DISPOSITION.md` and
  `docs/audits/REMOVALS.md`).
- No accidental nested git repos: verified (Pterodactyl imported
  via subtree merge, not as nested repos; verified by `git log`
  showing subtree merge commits).
- Upstream licensing and notices remain available: verified
  (`upstream/pterodactyl-panel/LICENSE.md` and
  `upstream/pterodactyl-wings/LICENSE` preserved verbatim;
  Chromium `LICENSE` verified present at upstream pinned
  revision; will be preserved when source is imported).

No regressions: there is nothing to regress (no upstream source
modified; no upstream tests run in this environment).

## 4. Architecture

### 4.1 Final component boundaries

Per `docs/architecture/ZIM_ARCHITECTURE.md`:

- **Browser (Chromium-derived):** owns tabs, windows, bookmarks,
  history, session restoration, profile separation, renderer/host
  process isolation. Authority for browser state.
- **Engine (Rust):** owns server lifecycle (creation, install,
  startup, supervision, console streaming, files, backups,
  transfers, scheduling, metrics, permissions, audit, remote-node
  coordination). Authority for server state.
- **Integration layer (Chromium-side C++):** connects the browser
  to the engine through stable, documented, capability-controlled
  contracts.
- **Default frontend (ZIM-authored):** the browser-native UI for
  managing servers, served as `zim://` internal pages.
- **Default extensions (ZIM-authored):** bundled ZIM addons using
  the ZIM addon SDK.

For each piece of state, exactly one component is authoritative
(see `ZIM_ARCHITECTURE.md` §1 table).

### 4.2 Rust engine design

Per `docs/architecture/RUST_ENGINE.md`:

- Cargo workspace under `zim/engine/` with proposed crate
  structure (domain, application, api, persistence-{sqlite,postgres},
  runtime-{docker,host}, node-runtime, streaming, scheduler, jobs,
  backups, transfers, files, eggs, extensions, auth, authz, audit,
  metrics, telemetry, platform, frontend-loader, migrate-pterodactyl,
  main).
- Permitted dependency direction: `domain` → no deps; `application`
  → `domain` + port traits; `api` → `application`; adapters →
  `application`'s ports + `domain`; `main` wires them together.
- Domain model: full typed Rust types for Instance, Node,
  Allocation, User, Permission, Job, Backup, Transfer, Schedule,
  AuditRecord, ExtensionRegistration. `PermissionGrant` enum
  preserves Panel's vocabulary verbatim.
- Supervision module: per-instance `Mutex<DesiredState>`,
  single-worker power queue, `Runtime` trait with `DockerRuntime`
  and `HostRuntime` adapters, `CrashState` with crash-loop
  detection, generation counter for stale-event protection.
- Persistence: SQLite default (WAL mode), Postgres optional for
  multi-node. Migrations are forward-only SQL.
- Streaming: per-instance subscriber set with replay-from-checkpoint,
  backpressure (drop oldest, increment counter).
- Scheduler: per-instance cron, `only_when_online` support,
  persisted across restarts.
- Jobs: durable, resumable state machines for backup, transfer,
  install; background cleaner for `cleanup_pending` jobs.
- Node-runtime contract: HTTPS+WSS, bidirectional, versioned,
  authenticated; engine sends desired state, node sends observed
  state + events.
- Frontend loader: smoke test on activation; reverts to default
  frontend on failure.

### 4.3 Chromium integration strategy

Per `docs/architecture/BROWSER_INTEGRATION.md`:

- `zim://` URL scheme (custom scheme) with documented URL
  inventory (`zim://newtab`, `zim://servers`, `zim://servers/{id}/console`,
  etc.).
- Per-tab server metadata via `TabFeatures` extension
  (`ServerTabFeature`).
- Server-tab context menu items via `TabMenuModel` extension.
- Browser-side IPC client (C++) talks to the engine via HTTPS+WSS
  on loopback, with per-tab capability tokens.
- Session restoration preserves browser state; never issues server
  lifecycle commands (invariant enforced at engine boundary).
- Split view uses Chromium 157's existing `split_tabs` API (no
  parallel implementation).
- NTP replaced by `zim://newtab` via WebUI registration (no
  upstream NTP source modification).
- 7 local Chromium patches identified (0001-0007) with per-patch
  source evidence, regression test, and expected breakage.

### 4.4 Frontend and extension customization model

Per `docs/api/FRONTEND_EXTENSION_SPEC.md`:

- Three customization levels: theme (token values only, no code),
  frontend package (page surfaces + permissions + signature),
  full frontend replacement (`full_replacement: true`).
- Theme tokens are values, not scripts; the engine refuses themes
  with non-literal token values.
- Frontend packages are signature-required (Ed25519 preliminary);
  trusted-author key set curated; unknown-author packages require
  explicit admin approval.
- Smoke test on activation: manifest schema, API version compat,
  entrypoint parse, configured smoke-test page execution.
- Recovery: broken custom frontend reverts to default frontend;
  recovery notice surfaced.
- ZIM addons are a separate ecosystem from Chromium MV3 extensions;
  permissions declared in manifest, checked at engine boundary on
  every API call.
- Addon capabilities: page surfaces, sidebar items, context menu
  items, commands with accelerators, event subscriptions,
  dependencies (resolved at install).
- Error isolation: each addon's page runs in its own renderer
  process; a crashing addon doesn't affect others or the default
  frontend.
- Native browser surfaces not replaceable: tab strip behavior,
  omnibox, bookmark bar, window chrome, reserved keyboard
  shortcuts.

### 4.5 Security and lifecycle invariants

Per `docs/architecture/ZIM_ARCHITECTURE.md` §4 (16 lifecycle
invariants) and `docs/security/THREAT_MODEL.md` (16 attack
scenarios):

- 16 lifecycle invariants, each with an enforcement point in the
  engine and a corresponding test in
  `docs/architecture/TEST_STRATEGY.md` §2.
- 16 attack scenarios, each with mitigations and residual risks.
- 6 trust tiers with explicit capability boundaries.
- Authentication: session cookies (HttpOnly/Secure/SameSite=Strict),
  API keys (Argon2id hashed), tab capability tokens (short-lived,
  revoked on tab close).
- Authorization: capability check at API boundary on every
  request; manifests are not proof of authorization.

## 5. Validation

### 5.1 Commands or checks actually run

| Check                                              | Result                | Notes |
|-----------------------------------------------------|-----------------------|-------|
| Workspace baseline (df, free, git --version, etc.)  | Run                   | See `docs/audits/BASELINE.md`. |
| Org repo listing via GitHub API                     | Run                   | Confirmed 1 repo (ZIM), 0 private, plan: Free. |
| ZIM repo branches via GitHub API                    | Run                   | Confirmed `main` branch with 1 commit. |
| Pterodactyl Panel tags via GitHub API               | Run                   | Confirmed latest tag `v1.15.1` (sha `e98678c5a1d0`). |
| Pterodactyl Wings tags via GitHub API               | Run                   | Confirmed latest tag `v1.13.3` (sha `6987d5e6f061`). |
| Chromium tag ref via GitHub API                     | Run                   | Confirmed `157.0.8103.0` → `b0a05135bf5a60bb3ab5a1e86a0e55193cb126d0`. |
| `git subtree add` Panel                             | Run                   | Subtree-merge commit `2359256de` on ZIM `main`. |
| `git subtree add` Wings                             | Run                   | Subtree-merge commit `4074be838` on ZIM `main`. |
| `git log --oneline` count                           | Run                   | 6265 commits (includes full Pterodactyl history). |
| `git ls-files \| wc -l`                             | Run                   | 1665 files in working tree. |
| `du -sh .git`                                       | Run                   | 30 MB. |
| `du -sh upstream`                                   | Run                   | 16 MB. |
| GitHub contents API for `chromium/chromium/157.0.8103.0` (top-level) | Run | All expected top-level dirs confirmed. |
| GitHub contents API for `chrome/browser/` at pinned revision | Run | 594 entries confirmed. |
| GitHub contents API for `chrome/browser/ui/tabs/` | Run | Full tab infrastructure confirmed including 8 `split_*` files. |
| GitHub contents API for `chrome/browser/sessions/` | Run | Full session restore infrastructure confirmed. |
| GitHub contents API for `chrome/browser/extensions/` | Run | 611 entries confirmed. |
| GitHub contents API for `content/public/browser/`   | Run | 695+ entries confirmed. |
| Raw HTTPS fetch of `chrome/browser/ui/tabs/tab_strip_model.h` | Run | 1551 lines, `split_tabs` namespace verified. |
| Raw HTTPS fetch of `chrome/browser/ui/browser.h`   | Run | 303 lines, `Browser` class inheritance + ownership verified. |
| Raw HTTPS fetch of `chrome/browser/ui/tabs/split_tab_util.h` | Run | 33 lines. |
| Raw HTTPS fetch of `chrome/common/url_constants.h` | Run | 749 lines. |
| Raw HTTPS fetch of `DEPS`                           | Run | 4978 lines. |
| Raw HTTPS fetch of `.gn`                            | Run | 479 lines. |
| Local file reads of Panel: `app/Services/Servers/ServerCreationService.php` | Run | Constructor injection + handle() flow verified. |
| Local file reads of Panel: `app/Models/Server.php` | Run | Full `@property` list extracted. |
| Local file reads of Panel: `app/Models/Permission.php` | Run | Permission constants extracted. |
| Local file reads of Panel: `routes/api-remote.php` | Run | 11 Wings↔Panel endpoints documented. |
| Local file reads of Wings: `server/server.go`       | Run | `Server` struct fields verified. |
| Local file reads of Wings: `server/power.go`        | Run | `PowerAction` enum + serialization invariant verified. |
| Local file reads of Wings: `environment/environment.go` | Run | `ProcessEnvironment` interface + 4-state model verified. |
| Local file reads of Wings: `server/crash.go`        | Run | `CrashHandler` + crash-detection gating verified. |
| Local file reads of Wings: `server/install.go`      | Run | Docker-containerized install verified. |
| Local file reads of Wings: `server/websockets.go`   | Run | `WebsocketBag` multi-subscriber model verified. |
| Local file reads of Wings: `server/manager.go`      | Run | `Manager` struct verified. |
| Local file reads of Wings: `environment/docker.go` | Run | `Docker()` singleton + network setup verified. |
| Local file reads of Wings: `go.mod`                  | Run | Go 1.24, full dependency list extracted. |
| `git ls-files | wc -l` (combined working tree)      | Run                   | 1665 files. |
| Local file reads of upstream Pterodactyl Panel + Wings source trees | Run | See PTERODACTYL_ARCHITECTURE.md for citation list. |

### 5.2 Tests passed and failed

- **Tests passed:** none (no test suites run in this environment —
  no Rust toolchain for engine tests, no Chromium build for
  browser tests, no PHP runtime for Panel tests, no Go toolchain
  for Wings tests). All tests are identified in
  `docs/architecture/TEST_STRATEGY.md` with status "Planned".
- **Tests failed:** none (no tests run).

### 5.3 Build readiness

- **Pterodactyl Panel build:** not run (no PHP runtime; `composer
  install` not attempted). The Panel source is preserved as audit
  reference; it is not built by ZIM.
- **Pterodactyl Wings build:** not run (no Go toolchain). The
  Wings source is preserved as audit reference; it is not built
  by ZIM.
- **Chromium build:** not run (no `depot_tools`, insufficient
  disk, insufficient memory). Documented as blocker B2/B3/B4 in
  `docs/architecture/IMPLEMENTATION_ROADMAP.md`.
- **ZIM engine build:** not run (no Rust toolchain). Blocker B1.
- **ZIM integration build:** not run (depends on Chromium build).

### 5.4 Environment limitations

- **Disk:** 9.9 GB total, ~9.2 GB free after Pterodactyl imports.
  Insufficient for full Chromium source + deps (~100 GB).
- **Memory:** 4.1 GB total. Insufficient for a Chromium release
  build's link step (tens of GB).
- **Toolchain:** no Rust toolchain, no `depot_tools`, no Go, no
  PHP runtime. ZIM inherits these limitations for the architecture
  phase; the implementation phase requires a provisioned build
  environment (see
  `docs/architecture/IMPLEMENTATION_ROADMAP.md` M1).
- **Docker:** not installed in the workspace. Engine integration
  tests that require Docker are blocked (B5).

## 6. Remaining work

### 6.1 Specific blockers

| Blocker                                    | Effect                                                            | Mitigation |
|--------------------------------------------|-------------------------------------------------------------------|------------|
| B1: no Rust toolchain                       | All engine work blocked                                           | Install rustup; M1. |
| B2: no `depot_tools`                        | Chromium source import blocked                                    | Install depot_tools; M1. |
| B3: insufficient disk for Chromium source   | Chromium source import blocked                                    | Provision build environment with ≥200 GB disk; M1. |
| B4: insufficient memory for Chromium build  | Chromium build blocked                                            | Provision build environment with ≥32 GB RAM; M1. |
| B5: no Docker runtime                       | Engine integration tests for DockerRuntime blocked                 | Install Docker Engine / Docker Desktop; M1. |
| B6: 7 INVESTIGATE items in component-disposition | Specific implementation tasks blocked for items requiring full source reading | Clear each INVESTIGATE item during corresponding implementation milestone. |

### 6.2 Unresolved architectural decisions

Per `docs/architecture/DECISIONS.md` §2 (12 unresolved questions):

1. Exact `zim://` scheme registration mechanism (INVESTIGATE).
2. Exact `TabFeatures` attachment API (INVESTIGATE).
3. Exact `BrowserWindowFeatures` API surface (INVESTIGATE).
4. Initial database choice (SQLite vs. Postgres).
5. Remote-node contract wire format (gRPC vs. HTTPS+WSS).
6. `zim.toml` server-definition format's exact schema.
7. Frontend package signature algorithm (Ed25519 vs. RSA-PSS).
8. OAuth provider set (Discord first; second TBD).
9. Remote-node mutual authentication (mTLS vs. signed JWT).
10. WebAuthn (hardware security key) login support.
11. Audit log retention policy (preliminary: 365 days).
12. Linux without systemd support (Alpine / OpenRC).

### 6.3 The next implementation milestone and its acceptance criteria

Per `docs/architecture/IMPLEMENTATION_ROADMAP.md` §6:

**First Rust implementation milestone: M2 — Rust engine skeleton.**

**Interfaces:**
- `zim/engine/Cargo.toml` workspace manifest.
- One crate per proposed module in
  `docs/architecture/RUST_ENGINE.md` §1.
- Each crate's `lib.rs` declares its public types/traits (empty
  bodies acceptable; the goal is to establish the dependency graph).

**Tests:**
- `cargo build --workspace` succeeds.
- `cargo test --workspace` succeeds (no tests; no compile errors).
- `cargo clippy --workspace -- -D warnings` succeeds.
- `cargo fmt --check` succeeds.
- CI workflow runs all four on every push.

**Completion criteria:**
- The workspace compiles.
- The crate graph enforces the permitted dependency direction
  (verified by attempted `use` of a forbidden type failing to
  compile).
- CI is green on `main`.

**Pre-implementation requirements:** M1 (toolchain provisioning)
must be complete first.

## Conclusion

The architecture-phase assignment is complete. The repository at
`https://github.com/ZaminInstancesManager/ZIM.git` contains:

- Real upstream source trees for Pterodactyl Panel v1.15.1 and
  Pterodactyl Wings v1.13.3 (subtree-merged with full history
  preserved).
- A pinned Chromium revision (`157.0.8103.0`) with a documented
  deferred-import strategy and full constraint evidence.
- 18 engineering deliverables (audits, architecture, API specs,
  security, compatibility, platform, test strategy, upstream
  maintenance, roadmap, decisions) grounded in direct source
  inspection of all three upstream codebases.
- Zero unverified mass deletion of upstream source.
- Preserved user-authored content (Apache LICENSE, brand icon).
- An actionable 15-milestone implementation roadmap with the first
  Rust implementation milestone (M2) identified.

The architecture is established; a separate implementation phase
can now proceed without re-inventing the architecture as it goes.

---

## Addendum: Expanded scope (commit `5a61d1e1f`)

The "Additional Master Requirements for ZIM" appended to the
original prompt broaden ZIM from a Minecraft/game-server manager
to a general workload-management platform. The architecture was
extended additively to satisfy this expanded scope without
discarding the original-scope work. Two new documents and updates
to five existing documents were added:

**New documents:**

- `docs/architecture/WORKLOAD_ABSTRACTION.md` — canonical Workload
  model wrapping the existing Instance type; `WorkloadKind` enum;
  typed `WorkloadSpec` enum; nine capability traits
  (`ConsoleCapable`, `ReadyProbeCapable`, `HealthCheckCapable`,
  `FilesystemCapable`, `NetworkCapable`, `SnapshotCapable`,
  `Migratable`, `ExecCapable`, `MetricsCapable`) as optional
  `Runtime` extensions; capability discovery enforced at placement
  time; integration package manifest (`zim.toml`); five logical
  ownership layers.
- `docs/architecture/ACCEPTANCE_CRITERIA.md` — explicit
  demonstration of how the architecture satisfies each of the 10
  expanded-scope acceptance criteria.

**Updated documents:**

- `docs/architecture/ZIM_ARCHITECTURE.md` — added §9 (Workload
  generalization) reconciling the original server-flavored
  architecture with the expanded scope.
- `docs/architecture/RUST_ENGINE.md` — added §12 (Workload
  capabilities and typed adapter traits) with the capability
  trait inventory, capability discovery and enforcement model,
  and workload integration crate structure.
- `docs/architecture/DECISIONS.md` — added 7 new ADRs
  (AD-13 through AD-19) covering the expanded scope.
- `docs/audits/COMPONENT_DISPOSITION.md` — added §7 listing the
  6 ZIM-original workload integration crates.
- `README.md` — broadened product identity from Minecraft-specific
  to general workload-management platform.

**Architecture acceptance (per `ACCEPTANCE_CRITERIA.md`):** all
10 expanded-scope acceptance criteria are satisfied at the
architecture level; implementation verification is tracked in
`docs/architecture/IMPLEMENTATION_ROADMAP.md` (milestones M16-M19
cover workload integration implementation).

**Verification status:** all expanded-scope architecture work is
`Verified` at the documentation level (documents present on
remote, cross-references consistent, ADRs recorded). All
expanded-scope *implementation* is `Planned` (per the verification
discipline in `docs/architecture/TEST_STRATEGY.md` §7); the
expanded-scope invariant tests (E1-E16) are identified in
`docs/architecture/TEST_STRATEGY.md` §12.
