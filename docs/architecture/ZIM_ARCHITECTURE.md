# ZIM Architecture (Phase 6 — Authoritative)

This is the authoritative architecture document for ZIM. Other
architecture documents (`RUST_ENGINE.md`, `BROWSER_INTEGRATION.md`,
`PLATFORM_SUPPORT.md`, `TEST_STRATEGY.md`, `UPSTREAM_MAINTENANCE.md`,
`IMPLEMENTATION_ROADMAP.md`, `DECISIONS.md`) and the API documents
(`docs/api/ZIM_API_SPEC.md`, `docs/api/FRONTEND_EXTENSION_SPEC.md`)
elaborate specific aspects of this architecture; this document is
canonical for the boundaries between components.

## 1. Product and system boundaries

ZIM is a desktop product consisting of three independently-authoritative
layers:

1. **The browser** (Chromium-derived). Owns the user's navigation,
   tabs, windows, bookmarks, history, session restoration, profile
   separation, and renderer/host process isolation. The browser is
   the user's primary surface for everything they do, including
   server management.

2. **The engine** (Rust). Owns the lifecycle of every managed server:
   creation, configuration, install, startup, supervision, console
   streaming, file access, backups, transfers, scheduling, metrics,
   permissions, audit, and remote-node coordination. The engine is a
   long-running process independent of the browser; it survives
   browser crashes, renderer exits, and "all browser windows closed".

3. **The integration layer** (Chromium-side C++ +
   `zim://` WebUI + Rust-side HTTPS/WSS API). Connects the browser to
   the engine through stable, documented, capability-controlled
   contracts. The integration layer is replaceable on the *frontend*
   side (a ZIM frontend package can be swapped) and *versioned* on
   the *API* side (the engine's API has explicit version numbers).

The **default frontend** (authored by ZIM under `zim/frontend/`) is
the browser-native UI for managing servers. It runs as a set of
`zim://` internal pages in the browser. It is replaceable per
`docs/api/FRONTEND_EXTENSION_SPEC.md`.

The **default extensions** (under `zim/extensions/`) are bundled ZIM
addons that contribute additional UI surfaces (database management,
custom dashboards, etc.). They use the ZIM addon SDK, not the
Chromium extension platform, for any ZIM server-control permission.

### Per-component ownership

For each piece of state, exactly one component is authoritative. The
"one source of truth per responsibility" rule from the prompt is
enforced by these boundaries:

| State                                              | Authority              | Notes |
|----------------------------------------------------|------------------------|-------|
| Tab list, window list, bookmarks, history, session | Chromium              | Restored on restart; never re-derived from engine state. |
| Profile separation, cookies, origin storage        | Chromium              | Per-profile data owned by Chromium. |
| Server lifecycle (running, stopped, crashed)        | Rust engine            | A persisted `running` flag in the DB is *not* proof of liveness; the engine verifies liveness by talking to the runtime. |
| Server configuration (desired state)               | Rust engine's DB       | Source of truth for "what the server should be". |
| Server runtime state (observed state)              | Runtime adapter        | Source of truth for "what the server is". |
| Per-instance operation ordering                    | Rust engine            | Per-instance power-action queue (single worker). |
| File contents in a server's data directory         | The server's process   | The engine provides file API but does not own file contents in the running server's data directory; it mediates access. |
| Backups                                             | Rust engine            | A backup is a durable job with an identity; its bytes are stored where the engine put them. |
| User accounts                                       | Rust engine            | Local user accounts (with optional OAuth). |
| Per-subuser-per-server permissions                 | Rust engine            | Capability checked at the engine API boundary, not in the frontend. |
| Audit log                                           | Rust engine            | Append-only; the engine is the only writer. |
| Console stream                                      | Rust engine            | Fan-out from runtime → engine → subscribers; a tab is a subscriber. |
| ZIM extension manifests                            | Rust engine            | The manifest declares permissions; the engine validates each API call. |
| Default frontend package                           | Rust engine (loader)   | The engine loads the active frontend package on browser request; a broken frontend cannot prevent engine access. |

## 2. Process and runtime topology

At runtime, ZIM consists of the following processes:

```
┌───────────────────────────────────────────────────────────────────┐
│  Browser process (Chromium-derived)                               │
│  ├ BrowserManagerService (owns Browser windows)                    │
│  ├ TabStripModel per Browser window                                │
│  ├ TabFeatures (per-tab; ZIM ServerTabFeature attaches here)       │
│  ├ WebUI registration for zim:// URLs                              │
│  └ ZIM Integration Layer (C++; lives in the browser process)     │
│      ├ zim:// URL handler                                          │
│      ├ Frontend package loader (requests packages from engine)    │
│      └ Engine IPC client (HTTPS + WSS to local engine)            │
└───────────────────────────────────────────────────────────────────┘
              │ HTTPS + WSS (loopback, capability-scoped tokens)
              ▼
┌───────────────────────────────────────────────────────────────────┐
│  Engine process (Rust)                                             │
│  ├ HTTP + WS listener (loopback only)                             │
│  ├ API surface (docs/api/ZIM_API_SPEC.md)                          │
│  ├ Authz (capability enforcement at API boundary)                 │
│  ├ Domain services (servers, allocations, nodes, eggs, ...)       │
│  ├ Supervision module (per-instance lifecycle + power queue)     │
│  │   ├ Instance A: powerLock + Runtime (Docker / Host / Remote)   │
│  │   ├ Instance B: ...                                             │
│  │   └ Instance C: ...                                             │
│  ├ Streaming module (console + metrics fan-out to subscribers)   │
│  ├ Scheduler (per-instance cron)                                  │
│  ├ Jobs (backups, transfers, installs — durable, resumable)      │
│  ├ Persistence (DB + migrations)                                  │
│  └ Node runtime contract (for remote nodes)                       │
└───────────────────────────────────────────────────────────────────┘
              │ Versioned, authenticated, capability-scoped contract
              ▼
┌───────────────────────────────────────────────────────────────────┐
│  Remote node process (Rust, on a remote Linux machine)             │
│  ├ Runtime adapters (Docker; possibly host)                       │
│  └ Node-side supervisor (per-instance, talks to engine)          │
└───────────────────────────────────────────────────────────────────┘
```

Key properties:

- The **browser and engine are separate OS processes.** The engine
  does not run in the browser process. A browser crash does not stop
  the engine. A renderer crash does not stop the engine. Closing all
  browser windows does not stop the engine.

- The **engine does not depend on the active frontend's framework.**
  The frontend talks to the engine through the API; if the frontend
  fails to load, the engine is still reachable via the recovery
  frontend (the default frontend restored by the engine).

- The **engine does not hold HTTP requests open indefinitely.**
  Long-running operations (install, backup, transfer) are durable
  jobs with identities; the client receives a job ID immediately and
  subscribes to progress events via WebSocket.

- **Loopback-only by default.** The engine listens on `127.0.0.1`
  (or a Unix-domain socket on Linux/macOS). Remote-node
  communication is over a separate, authenticated, versioned
  contract, not over the loopback surface.

- **No second backend.** There is no Panel-style PHP server behind
  ZIM. The engine is the backend. The browser is the frontend
  (replaceable).

## 3. Component boundaries

The ZIM source tree is organized under `zim/`:

```
zim/
├── engine/          # Rust workspace (see RUST_ENGINE.md)
├── contracts/       # Versioned IPC/API contracts (schema, OpenAPI)
├── frontend/        # Default frontend package (HTML/CSS/TS)
├── extensions/      # Bundled ZIM addons
└── integration/     # Browser-side C++ that connects Chromium to engine
    ├── webui_registration/   # zim:// scheme registration
    ├── server_tab_feature/   # per-tab ZIM metadata
    ├── tab_menu/             # server-tab context-menu items
    └ engine_ipc_client/     # HTTPS+WSS client to engine
```

### Dependency direction

Per the prompt's rule "the domain model must not depend on HTTP
handlers, frontend packages, Docker-specific types or operating-system
process APIs":

```
zim_engine::domain  ← (no deps on lower layers)
       ▲
       │
zim_engine::application  ← uses domain; defines service operations
       ▲
       │
zim_engine::adapters::{http, persistence, runtime, ...}  ← implements application's ports
       ▲
       │
zim_engine::api  ← thin HTTP/WS handlers; calls application services
```

`zim_engine::domain` has zero dependencies on HTTP, persistence, or
runtime types. `zim_engine::application` defines service operations
using domain types and *ports* (Rust traits) for persistence and
runtime; concrete adapters implement those traits. `zim_engine::api`
is a thin layer that translates HTTP/WS to application service calls
and enforces capability checks.

The browser-side `zim/integration/` is C++ in the Chromium tree; it
depends on Chromium headers and on the engine's HTTPS/WSS API (via
the IPC client). It does not depend on the engine's Rust source.

### The contracts directory

`zim/contracts/` contains the canonical, versioned:

- OpenAPI schema for the HTTP API.
- AsyncAPI schema for the WebSocket event contract.
- JSON Schema for the frontend package manifest.
- JSON Schema for the ZIM addon manifest.
- JSON Schema for the egg-importer input format.
- TOML schema for the `zim.toml` server-definition format.

These are the source of truth. Rust types are generated from them
where possible; the C++ IPC client reads them as documentation.

## 4. Lifecycle correctness invariants

The prompt requires invariants that can be enforced by code and tested
under failure. These are the canonical ZIM lifecycle invariants; each
is enforced at a specific point in the engine and verified by a
specific test (see `docs/architecture/TEST_STRATEGY.md`).

1. **A persisted `running` flag is not proof that a process is alive.**
   Enforcement: on engine startup, the engine reconciles every
   instance's persisted state with the runtime's observed state
   (calls `runtime.state()` for each instance). Instances whose
   persisted state says "running" but whose runtime says "offline"
   are marked crashed (or stopped, depending on policy), not
   running. Test: `engine_integration::reconcile_after_restart`.

2. **An accepted start request is not proof that the server started.**
   Enforcement: a start request returns a job ID immediately; the
   job transitions through `Pending → PullingImage → Starting →
   Running` (or `Failed` at any point). The HTTP response to the
   start request reports `accepted`, not `running`. Test:
   `engine_integration::start_job_state_machine`.

3. **A tab's existence is not proof that a server should run.**
   Enforcement: the engine has no knowledge of tabs. Tabs are
   Chromium's concern. The engine's authoritative server state is
   not derived from tab presence. Test: invariant test — open a
   server tab; close it; verify the server is still running.

4. **Closing a tab does not stop its server.**
   Enforcement: a tab closing only unsubscribes that tab's WS
   connection from the engine's stream; it does not call any
   lifecycle API. Test: invariant test.

5. **Restoring a session does not issue server lifecycle commands.**
   Enforcement: session restore re-navigates tabs to `zim://` URLs;
   the loaded pages query the engine for current state and
   subscribe. No lifecycle API is called as a side effect of session
   restore. Test: invariant test.

6. **Multiple tabs cannot accidentally create multiple server processes.**
   Enforcement: a server is identified by its instance ID; the
   engine's supervision module has one `Instance` per ID; opening
   three tabs to the same instance's console creates three WS
   subscribers to one instance's stream, not three instances. Test:
   invariant test.

7. **Lifecycle operations cannot race into undefined states.**
   Enforcement: each instance has a single-worker power-action
   queue (mirroring Wings's `powerLock`); concurrent
   start/stop/restart/kill requests are sequenced, not raced. Test:
   fault-injection test (`engine_integration::power_action_race`).

8. **A native process runner is not equivalent to an isolated container.**
   Enforcement: the `Runtime` trait exposes a `kind()` method
   returning `Container` (Docker) or `Host` (no isolation); the
   engine's security policy treats them differently — host runtime is
   opt-in per instance and requires explicit admin consent. Test:
   `engine_integration::host_runtime_requires_consent`.

9. **Per-instance operation serialization.**
   Enforcement: per-instance mutex + single-worker power queue
   (Wings's invariant, preserved in Rust). Test: fault-injection.

10. **Engine-restart reconciliation.**
    Enforcement: on startup, the engine reconciles persisted state
    with runtime observed state for every instance; in-flight
    durable jobs (backups, transfers, installs) are resumed if
    incomplete, or rolled back if non-resumable. Test:
    `engine_integration::restart_reconciliation`.

11. **Cleanup after partial failures.**
    Enforcement: every long-running operation is a durable job with
    an explicit state machine; partial failures leave the job in a
    recoverable state (with `cleanup_pending` flag); a background
    cleaner runs until cleanup completes. Test: fault-injection.

12. **Deletion and transfer coordination.**
    Enforcement: deletion acquires the instance's power lock, stops
    the process (if running), waits for the stop job to complete
    (with deadline), then deletes files. Transfer uses a two-phase
    commit (source: stop + archive; dest: extract + start; on
    success: source: delete). Test: fault-injection.

13. **Stale event protection.**
    Enforcement: every event from the runtime carries the runtime's
    observed generation counter; the engine ignores events whose
    generation is older than the current generation (prevents
    "zombie" events from a stopped process being interpreted as
    current state). Test: fault-injection.

14. **Crash-loop protection.**
    Enforcement: an instance that crashes N times within window T
    is placed in `crash_loop` state and is not auto-restarted; user
    intervention is required to clear the state. Test:
    `engine_integration::crash_loop_protection`.

15. **Capability checks at the engine boundary, not in the frontend.**
    Enforcement: every API call is checked against the caller's
    capability token; the frontend cannot bypass the check because
    it does not have direct DB or runtime access. Test:
    `engine_integration::capability_enforcement`.

16. **Restoration of default frontend on custom-frontend failure.**
    Enforcement: the engine's frontend loader checks the active
    frontend package's manifest and runs a smoke test on load; if
    the smoke test fails, the engine reverts to the default frontend
    and surfaces a recovery notice. Test: E2E.

## 5. Failure behavior

The prompt requires explicit failure behavior. For each failure mode,
ZIM's response:

| Failure                              | Engine behavior                                                                  | Browser behavior                                |
|---------------------------------------|----------------------------------------------------------------------------------|-------------------------------------------------|
| Renderer crash                        | Unaffected; the engine has no renderer dependency.                              | Chromium restores the renderer; tab navigates back to its `zim://` URL; the loaded page re-subscribes to the engine. |
| All browser windows closed            | Unaffected; the engine is an independent process.                               | (none — no browser to behave)                  |
| Browser process crash                 | Unaffected.                                                                      | On restart, `exit_type_service` records a crash exit; Chromium offers session restore; restored tabs re-subscribe. |
| Engine process crash                  | The engine restarts (managed by the OS service manager / launcher). On startup, it reconciles persisted state with runtime state for every instance; durable jobs resume or roll back. | The browser's IPC client detects the engine is unreachable; surfaces a recovery notice; WS subscriptions automatically reconnect with replay-from-checkpoint semantics when the engine returns. |
| DB transaction failure                | The engine returns a structured error to the caller; the operation is rolled back; no partial state persists. | The browser's page surfaces the error to the user. |
| Remote node disconnect                | The engine marks the node's instances as `unknown` (NOT `stopped`); the engine does not assume the servers have stopped. The node reconnects via the runtime contract; on reconnect, the engine reconciles. | Server tabs for instances on the disconnected node show `unknown` status (gray). |
| Custom frontend package fails to load | The engine's frontend loader reverts to the default frontend; a recovery notice is surfaced. | The browser loads the default frontend; the user sees the notice. |
| Custom extension package fails to load | The engine's extension loader disables the extension; the rest of ZIM continues. | The extension's UI surfaces disappear; the user sees a notice in `zim://extensions`. |
| Malformed `zim://` URL                 | The browser's WebUI handler returns a 404 / "not found" internal page; no engine call is made. | (none — the browser handles it). |
| Missing server ID in a `zim://` URL    | The browser's WebUI handler returns a "this server does not exist" page; the engine's `instances/{id}` API returns 404. | The user sees the not-found page. |
| Deleted instance                      | The engine returns 404; the browser's server-tab page shows "deleted" with options to close the tab. | The tab can be closed; the server is gone. |
| Unauthorized request                  | The engine returns 401/403; the browser's page surfaces a permission-denied notice. | The user sees the notice. |
| Container runtime unavailable          | The engine's `DockerRuntime` reports `Unavailable`; instance lifecycle operations fail with `runtime_unavailable`; the engine surfaces the failure to the user. | The user sees the failure; can retry. |
| Resource exhaustion (disk/memory)     | The engine refuses new operations with `resource_exhausted`; ongoing operations may fail and are placed in `failed` state. | The user sees the failure. |
| Update / rollback attack              | The engine refuses to load an unsigned newer frontend/extension package; the existing version is kept. | The user sees an update-failed notice. |

## 6. How this architecture satisfies the product vision

- **Manage servers through browser tabs** — The browser owns tabs
  (Chromium's `TabStripModel`); the engine owns server lifecycle
  (independent process); the integration layer attaches server
  metadata to tabs via `TabFeatures`. Opening a Paper server's
  console in three tabs creates three views, not three processes.

- **Single, integrated product** — ZIM is one product: a
  Chromium-derived browser with an integrated Rust engine. The
  Pterodactyl Panel/Wings split is replaced by the single engine.

- **Replaceable frontend** — The default frontend is a package loaded
  by the engine on browser request; custom frontends can replace it
  per `docs/api/FRONTEND_EXTENSION_SPEC.md`. The engine's runtime
  independence guarantees a broken frontend cannot lock the user out.

- **Extensible addon ecosystem** — ZIM addons declare permissions in
  their manifest; the engine validates each API call against its
  capability store. Chromium MV3 extensions do not auto-inherit ZIM
  server-control permissions.

- **Operational if Chromium crashes** — The engine is an independent
  process; it survives browser crashes, renderer exits, frontend
  failures, and "all windows closed".

- **Behavior-compatible with Pterodactyl where it matters** —
  Permission vocabulary, four-state lifecycle model, power-action
  vocabulary, egg format, and (deferred) legacy client API surface are
  preserved.

## 7. Architectural decisions (summary)

The detailed ADRs are in `docs/architecture/DECISIONS.md`. Summary:

- **AD-1:** ZIM is a desktop product, not a hosted panel.
- **AD-2:** Chromium is the browser foundation (not Gecko, not WebKit).
- **AD-3:** The engine is Rust (not Go, not Node, not PHP).
- **AD-4:** Browser and engine are separate processes (not in-process).
- **AD-5:** The engine's API is HTTPS+WSS on loopback (not a custom
  IPC, not DBus, not named pipes).
- **AD-6:** The frontend is replaceable via a package manifest (not
  hardcoded into the engine).
- **AD-7:** ZIM extensions are distinct from Chromium extensions; ZIM
  permissions are checked at the engine boundary, not in the
  manifest.
- **AD-8:** The Pterodactyl Panel/Wings split is replaced by a single
  engine; no transitional dual-backend.
- **AD-9:** Chromium source import is deferred until a build
  environment with sufficient capacity is provisioned.
- **AD-10:** Pterodactyl Panel/Wings source is preserved as audit
  reference, not used in the runtime.

## 8. Open architectural questions

Tracked in `docs/architecture/DECISIONS.md` §unresolved:

1. The exact `zim://` scheme registration mechanism (custom scheme
   vs. WebUI extension) needs source-backed investigation once
   Chromium is imported.
2. The exact `TabFeatures` attachment API for per-tab server metadata
   needs source-backed investigation.
3. The choice of initial database (SQLite vs. PostgreSQL) for
   single-machine vs. multi-node deployments is open; the
   architecture supports both via the `persistence` port.
4. The remote-node runtime contract's exact wire format (gRPC vs.
   HTTPS+WSS) is open.
5. The `zim.toml` server-definition format's exact schema is open;
   the legacy egg importer is the compatibility bridge.

## 9. Workload generalization (expanded scope)

The original architecture above is framed in terms of "servers"
and "instances" — the Pterodactyl/Panel vocabulary that ZIM
inherited. The expanded product vision broadens ZIM from a
game-server manager to a general workload-management platform.
This section reconciles the original server-flavored architecture
with the expanded scope.

The canonical Workload model is in
`docs/architecture/WORKLOAD_ABSTRACTION.md`. The architecture
acceptance criteria demonstrating that the expanded scope is
satisfied are in `docs/architecture/ACCEPTANCE_CRITERIA.md`.

### 9.1 Relationship between Instance and Workload

`Instance` (in `docs/architecture/RUST_ENGINE.md` §3) remains the
canonical type for Pterodactyl-compatible fields (uuid,
uuidShort, external_id, name, status, owner_id, memory, swap,
disk, io, cpu, threads, oom_disabled, allocation_id, egg_id,
startup, image, allocation_limit, database_limit,
backup_limit, crash_detection_enabled, installed_at). This type
is preserved to maintain the Pterodactyl migration importer's
contract verbatim.

`Workload` (in `docs/architecture/WORKLOAD_ABSTRACTION.md` §3) is
the broader abstraction. A `Workload` row references an `Instance`
row; the Instance carries Pterodactyl-shape fields. A workload
of kind `GameServer` uses essentially all Instance fields; a
workload of kind `Database` uses a subset (e.g. `egg_id` is null
because databases don't have eggs; `startup` is null because
databases use `DatabaseSpec.init_script_ref` and
`DatabaseSpec.readiness_probe` instead).

### 9.2 Five logical layers

Per the expanded scope §3, the architecture is re-organized into
five logical ownership layers (documented in
`docs/architecture/WORKLOAD_ABSTRACTION.md` §2):

1. Browser platform (Chromium-derived).
2. ZIM user experience (frontend + extensions + browser-side
   integration).
3. ZIM engine (Rust; authoritative state, lifecycle, persistence).
4. Runtime adapters (Docker / host / remote).
5. Workload integrations (Minecraft, database, website, dev
   process, generic container, custom).

These are *logical* boundaries, not five processes. The browser
(layer 1) and engine (layer 3) are separate OS processes; layers
2, 4, and 5 are in-process modules within their respective
processes.

### 9.3 Capability-trait model

The expanded scope §2 explicitly states the engine must not
assume every workload:
- Has a console accepting text commands.
- Is represented by one executable.
- Has one configuration file.
- Can be stopped safely by sending a generic termination signal.
- Uses the same networking or port-allocation model.
- Can be migrated by copying its files while running.
- Has the same definition, installation, or update procedure.

The capability-trait model in
`docs/architecture/WORKLOAD_ABSTRACTION.md` §4 satisfies this:
the `Runtime` base trait is the minimum (state, start, stop, kill,
exec, generation, kind); the capability traits (`ConsoleCapable`,
`ReadyProbeCapable`, `HealthCheckCapable`, `FilesystemCapable`,
`NetworkCapable`, `SnapshotCapable`, `Migratable`, `ExecCapable`,
`MetricsCapable`) are optional. Unsupported operations return
`Unsupported { reason }`, not a silent no-op or fake success.

### 9.4 Workload-aware tabs and URL inventory

The `zim://` URL inventory in
`docs/architecture/BROWSER_INTEGRATION.md` §2 is updated to use
workload terminology (`zim://workloads/{id}/...`). Legacy
`zim://servers/{id}/...` URLs continue to work as back-compat
aliases (documented in `docs/api/ZIM_API_SPEC.md`).

A workload tab's status indicator, context menu, and default
surface are governed by the workload's `kind` and the
integration's declared `context_menu_items` and `default_page`
(per `docs/architecture/WORKLOAD_ABSTRACTION.md` §8).

### 9.5 Lifecycle generalization

The four-state base lifecycle (`Offline`, `Starting`, `Running`,
`Stopping`) is preserved (per
`docs/architecture/DECISIONS.md` AD-11). Each workload kind may
declare an *extended* lifecycle that adds kind-specific states
(`Recovering`, `Initializing`, `Deploying`, `Draining`,
`Watching`, etc.); these surface as substates of the four primary
visual statuses (per
`docs/architecture/WORKLOAD_ABSTRACTION.md` §6) — never
misrepresenting an extended state as one of the four primaries.

### 9.6 Expanded-scope ADRs

The expanded scope introduced new architectural decisions
AD-13 through AD-19, recorded in
`docs/architecture/DECISIONS.md` §1. Summary:

- **AD-13:** ZIM is a general workload-management platform, not
  a Minecraft-specific product.
- **AD-14:** The `Runtime` trait is split into a base trait +
  optional capability traits; unsupported operations report
  `Unsupported` explicitly.
- **AD-15:** Workload integrations are loaded as Rust libraries
  (dynamic loading OR build-time feature gating; the choice is
  AD-15 itself).
- **AD-16:** First-party workload integrations are bundled under
  `zim/engine/crates/workloads/{minecraft, database, website,
  dev_process, generic_container}/`; third-party integrations are
  loaded via the integration package manifest.
- **AD-17:** The `Workload` type wraps `Instance` rather than
  replacing it, preserving the Pterodactyl migration contract.
- **AD-18:** Capability discovery is enforced at workload
  placement time; the engine refuses to place a workload on a
  runtime that does not provide its required capabilities.
- **AD-19:** The expanded `zim://workloads/{id}/...` URL inventory
  is the canonical surface; `zim://servers/{id}/...` is a
  back-compat alias.
