# Implementation Roadmap (Phase 6)

This document specifies ZIM's implementation roadmap:
dependency-ordered milestones, the acceptance criteria for each
milestone, and the known blockers. It is the bridge between the
architecture phase (this assignment) and the implementation phase
(separate work).

It is canonical for "what happens next". The architecture documents
are canonical for "what the result looks like".

## 1. Implementation boundary for this assignment

This assignment (the architecture phase) does **not** authorize an
immediate, improvised rewrite of all Pterodactyl functionality in
Rust. The prompt is explicit:

> *"Do not begin a broad backend rewrite simply to demonstrate
> progress. ... Once the architecture is established, the roadmap
> must identify the first Rust implementation milestone, its
> interfaces, its tests and its completion criteria."*

The architecture is established (Phase 6 documents). The roadmap
below identifies the first Rust implementation milestone and its
completion criteria. Implementation begins after this assignment
is complete.

## 2. Known blockers (must be addressed before implementation)

These blockers were identified during the architecture phase and
must be resolved before the corresponding milestone can proceed.

### B1: No Rust toolchain in the workspace

The current workspace has no `rustc` or `cargo`. All engine work is
blocked on provisioning a Rust toolchain. This is the first blocker
to clear.

Resolution: install `rustup` and a stable Rust toolchain in the
implementation environment. Estimated effort: minutes.

### B2: No depot_tools in the workspace

Even with sufficient disk, a Chromium build requires `depot_tools`
(`gclient`, `gn`, `ninja`). This blocks the Chromium source import
and any Chromium-side patch.

Resolution: install `depot_tools` in the implementation environment
(only when the Chromium import milestone is reached). Estimated
effort: minutes to install; the build itself is the long pole.

### B3: Insufficient disk for Chromium source + dependencies

The current workspace has ~9 GB free; a full Chromium source +
dependencies checkout requires ~100 GB. This blocks the Chromium
source import.

Resolution: provision a build environment with at least 200 GB of
free disk for the Chromium source + dependencies + build outputs.
This is a provisioning task, not a code task.

### B4: Insufficient memory for Chromium link

The current workspace has 4 GB RAM; a Chromium release build's
link step can require tens of gigabytes. This blocks the Chromium
build.

Resolution: provision a build environment with at least 32 GB RAM
for the Chromium build. Same as B3, a provisioning task.

### B5: No Docker runtime in the workspace

Engine integration tests that exercise the `DockerRuntime` require
a Docker daemon. This blocks the runtime tests.

Resolution: install Docker Engine (or Docker Desktop) in the
implementation environment. Estimated effort: minutes.

### B6: INVESTIGATE items in the component-disposition matrix

Seven items in `docs/audits/COMPONENT_DISPOSITION.md` §5 are
marked INVESTIGATE. These require deeper source inspection before
the corresponding implementation work can proceed.

Resolution: clear each INVESTIGATE item by reading the cited
source file's full body during the corresponding implementation
milestone.

## 3. Milestones (dependency-ordered)

### M1: Toolchain provisioning

**Goal:** Provision the implementation environment with the
toolchains needed for subsequent milestones.

**Deliverables:**
- Rust toolchain installed (`rustup`, stable Rust, with the
  `wasm32-unknown-unknown` target for any frontend tooling).
- Docker Engine installed and running.
- (For Chromium later) `depot_tools` installed.
- A build environment with at least 200 GB disk and 32 GB RAM
  provisioned (cloud VM or local workstation).

**Acceptance criteria:**
- `cargo --version` works.
- `docker info` works.
- `df -h` shows ≥200 GB free on the build volume.
- `free -h` shows ≥32 GB RAM.

**Blockers cleared:** B1, B5.

**Estimated effort:** 1 day (mostly provisioning).

### M2: Rust engine skeleton

**Goal:** Establish the Rust workspace skeleton under
`zim/engine/` with the crate structure proposed in
`docs/architecture/RUST_ENGINE.md` §1, even if the crates are
initially empty.

**Deliverables:**
- `zim/engine/Cargo.toml` workspace manifest.
- `zim/engine/crates/{domain, application, api, ...}/Cargo.toml`
  for each proposed crate.
- `cargo build --workspace` succeeds (with empty crates).
- `cargo test --workspace` succeeds (no tests yet, but no compile
  errors).
- CI workflow that runs `cargo build --workspace` on every push.

**Acceptance criteria:**
- `cargo build --workspace` exits 0.
- `cargo clippy --workspace -- -D warnings` exits 0.
- `cargo fmt --check` exits 0.
- CI runs build on every push.

**Blockers cleared:** B1.

**Estimated effort:** 1 week.

### M3: Domain model + persistence (SQLite)

**Goal:** Implement the domain types in
`docs/architecture/RUST_ENGINE.md` §3 and the persistence port
with a SQLite adapter, with migrations and a migration runner.

**Deliverables:**
- `zim_engine_domain` crate with all the types in §3 (Instance,
  Node, Allocation, User, Permission, Job, Backup, Transfer,
  Schedule, AuditRecord, ExtensionRegistration).
- `zim_engine_application::ports::Persistence` trait.
- `zim_engine_persistence_sqlite` crate implementing the port with
  a `rusqlite` (or `sqlx`) backend.
- Migrations under
  `zim/engine/crates/persistence-sqlite/migrations/`.
- Migration runner.
- Unit tests for each domain type's serialization round-trip and
  for each persistence operation.

**Acceptance criteria:**
- All domain types compile and have `#[test]` round-trip tests
  that pass.
- SQLite persistence round-trip tests pass for every entity.
- Migration runner applies migrations forward-only without
  data loss.
- `cargo test -p zim_engine_domain` and `cargo test -p
  zim_engine_persistence_sqlite` pass.

**Blockers cleared:** B1, B6 (partial — domain types' field sets
are settled; INVESTIGATE items affecting the persistence schema
are not relevant here).

**Estimated effort:** 2-3 weeks.

### M4: Authentication + authorization

**Goal:** Implement local user accounts, session management, API
keys, and the authz layer at the API boundary.

**Deliverables:**
- `zim_engine_auth` crate with local user accounts (Argon2id
  password hashing), session management, OAuth callback support
  (Discord first; Google/GitHub later).
- `zim_engine_authz` crate with the `PermissionSet` /
  `PermissionGrant` enum from §3 and the capability-check
  function.
- HTTP middleware that extracts the credential, resolves the
  `PermissionSet`, checks required grants, and returns
  `403 permission_denied` on missing grants.
- API key issuance and revocation endpoints.
- Unit tests for credential issuance, session expiry, permission
  checks.

**Acceptance criteria:**
- A user can register (admin-created initially; self-registration
  is a later milestone), log in, log out.
- A user's session cookie is `HttpOnly`, `Secure`, `SameSite=Strict`.
- A user with `control.start` permission can call a `control.start`
  endpoint; a user without it cannot (returns 403).
- `cargo test -p zim_engine_auth` and `cargo test -p
  zim_engine_authz` pass.

**Estimated effort:** 2-3 weeks.

### M5: API surface (HTTPS, no streaming yet)

**Goal:** Implement the HTTPS API surface from
`docs/api/ZIM_API_SPEC.md` for the read operations and basic
write operations (no lifecycle, no streaming, no jobs yet).

**Deliverables:**
- `zim_engine_api` crate with HTTP handlers for the read
  operations (`GET /instances`, `GET /instances/{id}`, `GET
  /nodes`, `GET /eggs`, etc.).
- Loopback-only HTTP listener (`127.0.0.1:{port}` or Unix socket).
- `Host` header validation (DNS rebinding mitigation).
- Structured error response format.
- Pagination (cursor-based for audit; page-based for others).
- ETag + `If-Match` concurrency control.
- Idempotency-Key deduplication.
- Rate limiting.
- Integration tests for each endpoint.

**Acceptance criteria:**
- Each endpoint in §10 of the API spec has a corresponding
  handler and integration test.
- `Host` header validation blocks non-loopback hosts.
- Concurrency control via ETag works (returns 412 on mismatch).
- Idempotency-Key deduplication works for 24 hours.
- Rate limiting returns 429 with `Retry-After`.

**Estimated effort:** 3-4 weeks.

**Delivery (2026-10-14, commit "M5 (core)"):** Delivered as
specified, with the scope decisions below recorded for reviewers.

- Every §10 route has a handler: endpoints whose dependencies exist
  (auth, users + API keys, instances + subusers + allocations +
  schedules, nodes, job reads, audit, extensions listing, the
  `/workloads` alias per §10.17) are implemented and integration
  tested over real TLS sockets; endpoints declared in the spec but
  scheduled for later milestones (lifecycle M6, files M6, console
  and metrics M7, backup mutations M8, transfer initiation M8,
  schedule run M9, eggs M9, frontend packages M11, workload
  integrations M16, OAuth post-M5) answer `501 not_implemented`
  with their milestone in the error body (the §6.1 transitional
  vocabulary).
- The engine binary (`zim_engine`) composes the adapters and is
  runnable end-to-end: SQLite migrations, first-run root-admin
  bootstrap, persisted self-signed loopback certificate, HTTPS
  listener, graceful SIGTERM drain.
- Acceptance criteria verified by the api-crate integration suite
  (host validation, 406 versioning, ETag/If-Match 412s,
  idempotency replay + key-reuse rejection with body-hash
  fingerprints, 429s with `Retry-After`/rate-limit headers, cookie
  attributes, subuser grant gates) and the main-crate end-to-end
  smoke test.
- Additive surface (per spec §2's additive-change rule): `GET
  /api/v1/health`; two documented error-code mappings (`bad_request`
  for rejected `Host` headers, `bad_request` + HTTP 406 with
  supported versions for unsupported `Accept` versions).
- Known simplifications, each documented at its implementation site:
  job listing is instance-scoped in M5 (the global listing arrives
  with the job system in M8); instance deletion does not yet emit
  cleanup jobs (M6+; the runtime owns the containers);
  `Persistence::create_allocation` was added to the M3 port because
  allocation creation needs the store's auto-increment id rather
  than `put_allocation`'s explicit-id upserts.

### M6: Runtime abstraction + Docker runtime

**Goal:** Implement the `Runtime` trait and the `DockerRuntime`
adapter.

**Deliverables:**
- `zim_engine_application::supervision` module with the `Instance`
  struct, `power_queue`, `CrashState`, `generation` counter.
- `zim_engine_runtime_docker` crate implementing `Runtime` via
  `bollard` (Rust Docker client).
- Per-instance lifecycle operations: start, stop, restart, kill,
  state, exit_state.
- The pterodactyl0 network setup (matches Wings's behavior).
- Engine-restart reconciliation.
- Integration tests using a test Docker container.

**Acceptance criteria:**
- An instance can be started, observed Running, stopped,
  observed Offline.
- A crashed instance is detected (via `exit_state`'s exit code +
  oom_killed flag) and the crash-loop protection engages after N
  crashes in window T.
- Engine restart reconciles persisted state with runtime observed
  state for every instance.
- All lifecycle invariant tests in
  `docs/architecture/TEST_STRATEGY.md` §2 (invariants 1, 2, 7, 9,
  10, 13, 14) pass.

**Blockers cleared:** B5, B6 (partial — the `Runtime` trait's
full method set was INVESTIGATE; cleared by reading the full
`environment.go` body).

**Estimated effort:** 4-6 weeks (the supervision module is the
heart of the engine).

**Delivery (2026-10-14, commit "M6 (core)"):** Delivered as
specified, with the scope decisions below recorded for reviewers.

- The supervision module (`zim_engine_application::supervision`)
  ships the `Runtime`/`RuntimeFactory` ports, the per-instance power
  queue (single-worker, invariants 7/9), the crash monitor with
  crash-loop parking (invariant 14, threshold 3-in-60s matching
  Wings's `CrashDetection.Timeout`), the generation counter for
  stale-sample protection (invariant 13), and engine-restart
  reconciliation (invariants 1/10) including adoption of
  still-running containers and crash-gated restarts for
  was-desired-started ones.
- The Docker adapter (`zim_engine_runtime_docker`) implements the
  port over bollard 0.21 with Wings-parity semantics: recreate-on-
  start (`OnBeforeStart`), graceful-then-kill bounded stops
  (`WaitForStop(terminate=true)`), 404-tolerant stop/kill, the
  `pterodactyl0` bridge network with Wings's bridge options, the
  full container spec (readonly rootfs, `/tmp` tmpfs, cap-drop set,
  no-new-privileges, 988:988 container user, CFS quota from
  millicores, port bindings, `SERVER_IP` loopback→gateway rewrite),
  one-shot exec, and one-shot stats. Documented deviations: the
  network's IPAM subnet is auto-allocated (not Wings's fixed
  172.18.0.0/16) and IPv6 is off by default.
- The API surface implements §10.3 lifecycle: POST
  start/stop/restart/kill/suspend (202 accepted + generation, per
  invariant 2), and GET state now reports the supervisor's live
  observation (desired/observed/generation/crash-loop) with the
  persisted row as fallback. `kill` requires elevated
  `control.stop` (owner or admin — a documented reading of the
  spec's "(elevated)" note); suspend answers AdminInstance and
  gates starts with `instance_state_conflict`. Additive error code:
  `runtime_unavailable` (503) for enqueue-time backend failures.
- The engine binary composes the supervisor at boot: Docker probe →
  `DockerRuntimeFactory` (creating `pterodactyl0`) or the
  `NullRuntimeFactory` degraded mode (API-only; lifecycle answers
  503), a default local node bootstrapped on first run, and the
  reconciliation pass logged at startup. Instances created after
  boot enter supervision lazily on their first power request or
  state read (LocalLinux-node rows only).
- M6 acceptance criteria are verified by three suites: the
  supervision integration tests (mock runtime; invariants 1, 2, 7,
  9, 10, 13, 14), the runtime-docker integration suite (real
  daemon; start/stop/observe, kill exit 137, recreate-on-start,
  readonly-rootfs/tmpfs/port-binding/exec/stats, crash-loop E2E
  with captured exit codes, reconcile adoption — self-skipping
  where no Docker answers, run for real in the CI job
  `docker-integration`), and the API lifecycle tests over the mock
  runtime (202s, grant gates incl. the elevated kill and suspend
  conflicts, observed_by supervisor, persisted-status follow).
- Known simplifications, each documented at its implementation site:
  the M6 startup renderer substitutes only
  `{{SERVER_MEMORY}}`/`{{SERVER_PORT}}`/`{{SERVER_IP}}` (the full
  egg-variable surface arrives with M9's egg importer); crash
  observation is poll-based (1s default) pending M7's event-driven
  form; an exit while desired-started counts as a crash including
  clean exits (Wings's `DetectCleanExitAsCrash` default); unsuspend
  is not in the §10.3 surface (documented gap, reserved for a later
  additive revision); non-admin owners now receive the implicit
  grant set (all but `AdminInstance`) in `load_instance_for_caller`
  — recorded as an M6 fix of an M5 gap (owners were "implicitly
  authorized" in prose but held empty grant sets in code). The
  console simplification (attach/send_input answering `Unsupported`)
  was retired by M7.1: the Docker runtime ships the attach pump and
  stdin writer, the mock runtime an in-memory echo mode, and
  `RuntimeError::NotRunning` the stopped-target vocabulary.

### M7: Streaming (console + metrics)

**Goal:** Implement the streaming module with WebSocket fan-out,
replay-from-checkpoint, and backpressure.

**Status:** M7 (core) delivered — the hub, the `/api/v1/stream`
WebSocket endpoint, replay, and backpressure; M7.1 (the follow-up
issue #4) delivered the runtime side of the console pipe: the
Docker adapter's bollard attach pump (decoded lossy-UTF-8 lines
onto the hub, a persistent stdin writer for `send_input`, replace
semantics mirroring the crash monitor's arm slot), the supervision
layer's console pump (`instance.console_output` events in
generation order, flush on exit, no double-pumps across restarts),
the mock runtime's echo mode, and the retirement of the console
input endpoint's 501 mapping (a stopped target now answers the
retryable `409 state_conflict`).

**Deliverables:**
- `zim_engine_streaming` crate with `StreamSource`,
  `StreamHandle`, `StreamEvent`.
- WebSocket endpoint at `/api/v1/stream`.
- Per-tab capability token issuance (from the integration layer
  on the browser side; this milestone ships the engine-side
  support).
- Replay-from-checkpoint on reconnect.
- Backpressure (drop oldest, increment `dropped_events`).
- Integration tests for the streaming scenarios.

**Acceptance criteria:**
- A subscriber receives `instance.state_change` events on
  lifecycle transitions.
- A subscriber that disconnects and reconnects with
  `?since_generation={n}` receives the events it missed.
- A slow subscriber's queue fills up; oldest events are dropped;
  `dropped_events` counter increments.

**Estimated effort:** 2-3 weeks.

### M8: Jobs (backup, transfer, install)

**Goal:** Implement the durable jobs module for backups,
transfers, and installs.

**Status:** M8.1 (issue #5) delivered — the durable-job engine
and the backup pipeline: the `zim_engine_jobs` executor
(claim-then-run with per-instance serialization and a global
concurrency cap, cooperative cancellation, the
`CleanupPending` → cleaner → `Failed` failure path per invariant
11, boot-time orphan reconciliation, `job.progress` /
`job.completed` hub events, panic containment) and the
`zim_engine_backups` crate (tar+gzip archives with streaming
sha256 and atomic `.part` → rename finalization, digest-verified
escape-safe restore with an in-place content swap (the live
directory itself is never renamed — a running container's bind
mount pins it, so the workload sees the restored data
immediately), the
`JobKind::Backup` handler covering both directions, the
`BackupService` row lifecycle incl. limits and locks, and the
migration 0002 `jobs.payload` column that carries a restore
job's target). The §10.5 backup mutation family and
`POST /jobs/{id}/cancel` are live (the 501s retired), and the
global admin job listing replaced M5's instance-scoped-only one.
M8.2 (issue #6) delivered — the transfer pipeline: the
`zim_engine_transfers` crate (the `TransferStore` working-tree
layout, the `TransferService` validation + row lifecycle, and
the `JobKind::Transfer` handler walking the two-phase commit
`Pending → SourceStopping → SourceArchiving →
ArchiveTransferring → DestExtracting → DestStarting →
SourceCleanup → Succeeded`, with every phase boundary persisted
before its side effects, power-state restore on pre-commit
failure, digest-verified shipping, the in-place content-swap
commit shared with the backup restore, the node + allocation
rebind, and the cleaner's transfer-tree cleanup), the §10.9
transfer mutation family (`POST`/`DELETE
`/instances/{id}/transfer`, the 501s retired) with
`transfer_status` routed through the service, and the
companion-row lifecycle closed (a transfer cancelled straight
out of `Pending` never runs its handler — the service walks the
row itself, and the in-flight check treats a non-terminal row
whose job already ended as stale, so the instance is never
permanently untransferable). Engine-local execution this
milestone: both halves of the transfer tree live under the
engine's data root (the node-runtime contract later replaces the
local ship step). M8.3 (issue #8) delivered — the install
pipeline: the `zim_engine_installs` crate (the `InstallService`
validation + install-in-flight guard over the §10.3
`POST /instances/{id}/install` mutation, and the
`InstallJobHandler` running the egg's verbatim install script —
or its update script, with the install fallback, for
`JobKind::Update` — through the new `InstallRunner` port in the
application layer), the `DockerInstallRunner` adapter (a
constrained one-shot container per run: capabilities dropped,
`no-new-privileges`, read-only root filesystem, the unprivileged
container user, no published ports, network egress on the
instance network, the instance data directory as the only
writable bind; pulled with byte-level progress; killed and
removed on every exit path including timeout and cancellation),
the stop-then-run-then-stamp pipeline (a running workload is
stopped through the power queue first; `installed_at` is stamped
on success; the workload stays stopped — Pterodactyl reinstall
semantics), and the failure semantics (a failed script fails the
job with the exit code + output tail and `cleanup_pending =
false`: partial downloads in the data directory are retry state
for the next install, not cleaner garbage). The degraded
API-only engine binds the `NullInstallRunner` (installs fail
fast with a structured error); tests bind the scriptable
`MockInstallRunner`. M8 is complete.

**Deliverables:**
- `zim_engine_jobs` crate with the `Job` state machine and
  resumable semantics.
- `zim_engine_backups` crate (archive creation, storage, restore).
- `zim_engine_transfers` crate (two-phase commit state machine).
- Install job (runs egg install script in constrained container).
- Background cleaner for `cleanup_pending` jobs.
- Integration tests including fault-injection (interrupted
  backup, failed transfer).

**Acceptance criteria:**
- A backup job creates an archive, records the sha256, and
  appears in the backup list.
- An interrupted backup leaves the job in `Failed` state with
  `cleanup_pending=true`; the background cleaner deletes the
  partial archive.
- A transfer goes through the two-phase commit; partial failure
  leaves the job in `Failed` state without affecting the source
  instance's running state.

**Estimated effort:** 4-6 weeks.

### M9: Egg importer + scheduler

**Goal:** Implement the egg importer (legacy Pterodactyl JSON
format → ZIM egg store) and the scheduler (per-instance cron).

**Status:** Delivered (issue #7) — the egg importer and the
scheduler. The `zim_engine_eggs` crate: the domain `Egg` model
(labeled images, startup template, verbatim install/update
scripts, per-file config parser rules incl. unknown-kinds
preserved, variables with legacy validation rules), migration
0003 (`eggs` + `egg_variables` tables; `delete_egg` refuses while
instances reference the egg), the legacy PTDL_v1/v2 JSON importer
(lenient: unknown fields/features surface as warnings; the
double-encoded `config.files` string and the v1 single-image
fallback both handled; unresolved startup placeholders warn), the
ZIM-native `zim.toml` format in both directions (round-trip
fidelity tested), and the security gates (`priviledged_install_scripts`
/ `requires_root` rejected unless the admin explicitly opts in).
The §10.11 egg API surface is live: `GET/POST /eggs`,
`GET/PATCH/DELETE /eggs/{id}` (import deduplicates on the legacy
UUID — a re-import updates in place). The `zim_engine_scheduler`
crate: a driver loop feeding a deterministic `tick(now)` — 5-field
cron (a zero-seconds field prepended onto the `cron` crate's
6-field dialect), `next_run_at`/`last_run_at` walked forward,
freshly-seen schedules never fire for past instants, missed
instants coalesce into one fire, `only_when_online` gates on the
supervisor's observed state, actions dispatch through the power
queue and console input, and `POST /instances/{id}/schedules/{sid}/run`
answers immediately (retiring the 501). The runtime config-rewriting
module (the `beevik/etree` XML path) is deferred to the
M17 workload-integration milestones — the parser rules are
preserved, not yet applied.

**Deliverables:**
- `zim_engine_eggs` crate with the importer and the
  config-rewriting module (using `beevik/etree` equivalent in
  Rust).
- `zim_engine_scheduler` crate with `tokio-cron-scheduler` (or
  equivalent).
- Security-tight egg import (reject `priviledged_install_scripts`
  by default).
- Integration tests with fixture eggs.

**Acceptance criteria:**
- A legacy Pterodactyl egg can be imported; its variables,
  startup template, install script, and config parser rules are
  preserved.
- A schedule fires at the configured cron time and dispatches
  the configured action.
- A schedule with `only_when_online: true` is skipped when the
  instance is not running.

**Estimated effort:** 2-3 weeks.

### M10: Migration importer

**Goal:** Implement the Pterodactyl Panel database importer.

**Deliverables:**
- `zim_engine_migrate_pterodactyl` crate with the table mapping
  from `docs/compatibility/MIGRATION_AND_COMPATIBILITY.md` §2.3.
- CLI: `zim migrate pterodactyl --panel-mysql-dsn=...
  --zim-db=...`.
- Dry-run mode.
- Migration E2E test against a fixture Pterodactyl Panel
  deployment.

**Acceptance criteria:**
- A fixture Pterodactyl Panel MySQL database is imported into a
  ZIM SQLite database with no data loss (per the table mapping).
- The migration E2E test in
  `docs/compatibility/MIGRATION_AND_COMPATIBILITY.md` §7 passes.
- Dry-run mode produces a correct summary without writing.

**Estimated effort:** 2-3 weeks.

### M11: Frontend loader + default frontend (minimal)

**Goal:** Implement the engine's frontend loader and a minimal
default frontend that can render the new-tab page, the server
list, and a server's console.

**Deliverables:**
- `zim_engine_frontend_loader` crate with manifest validation,
  smoke test, recovery to default.
- `zim/frontend/` with a minimal default frontend (vanilla TS +
  Web Components; no framework).
- The minimum viable page surfaces: `zim://newtab`,
  `zim://servers`, `zim://servers/{id}/console`.

**Acceptance criteria:**
- The engine serves the default frontend's pages at `zim://`.
- A custom frontend package that fails its smoke test is
  rejected; the default is kept.
- The minimal default frontend can list servers, open a console,
  and display state changes in real time.

**Estimated effort:** 3-4 weeks.

### M12: Chromium source import + local patches

**Goal:** Import the Chromium source tree and apply the local
patch queue (patches 0001-0006).

**Deliverables:**
- `chromium/src/` populated via `git subtree add` at the pinned
  revision.
- `gclient sync` run against the pinned revision.
- Patches 0001-0006 applied via `chromium/PATCHES/series`.
- `gn gen out/Default` succeeds.
- `ninja -C out/Default chrome` succeeds (a buildable Chromium
  with ZIM patches).

**Acceptance criteria:**
- The Chromium build produces a ZIM-branded browser binary.
- `zim://newtab` loads in the built browser.
- The ZIM local patches are recorded in
  `docs/architecture/UPSTREAM_MAINTENANCE.md` §3.4 with the actual
  application details.

**Blockers cleared:** B2, B3, B4.

**Estimated effort:** 1-2 weeks (assuming the build environment
is provisioned; the actual `ninja` build can take hours).

### M13: Integration layer (C++ on the Chromium side)

**Goal:** Implement the browser-side integration layer under
`zim/integration/` and link it into the Chromium build.

**Deliverables:**
- `zim/integration/webui_registration/` — `zim://` scheme
  registration (uses patch 0001/0002).
- `zim/integration/server_tab_feature/` — `ServerTabFeature`
  (uses patch 0003).
- `zim/integration/tab_menu/` — ZIM tab menu items (uses patch
  0005).
- `zim/integration/engine_ipc_client/` — HTTPS+WSS client to
  the engine.
- `zim/integration/crash_keys/` — ZIM crash keys (uses patch
  0006).
- The built browser links the integration layer.

**Acceptance criteria:**
- A `zim://servers/{id}/console` tab loads the default frontend's
  console page.
- The tab's status indicator updates in real time as the
  instance's state changes.
- The tab's context menu shows ZIM items.
- Closing the tab does not stop the server (invariant 4).

**Estimated effort:** 4-6 weeks.

### M14: End-to-end tests + cross-platform packaging

**Goal:** Implement the E2E test suite and cross-platform
packaging.

**Deliverables:**
- E2E tests under `tests/e2e/` covering the scenarios in
  `docs/architecture/TEST_STRATEGY.md` §3.
- Windows packaging (installer, service registration, URL
  protocol registration).
- Linux packaging (deb, rpm, systemd user unit, .desktop file).
- macOS packaging (app bundle, launchd agent, Info.plist URL
  protocol registration).
- Cross-platform CI matrix (Linux x64, Windows x64, macOS arm64).

**Acceptance criteria:**
- All E2E tests pass on all three platforms.
- A user can install ZIM on each platform, create an instance,
  start it, view its console, and stop it.

**Estimated effort:** 4-6 weeks.

### M15: First usable ZIM release

**Goal:** Cut the first usable ZIM release.

**Deliverables:**
- Release binaries for Windows x64, Linux x64, macOS arm64.
- Release notes documenting what works and what doesn't.
- Migration guide for users coming from Pterodactyl.
- The release is marked "alpha" or "preview"; not production-ready.

**Acceptance criteria:**
- All M1-M14 milestones complete.
- All architectural invariant tests in
  `docs/architecture/TEST_STRATEGY.md` §2 pass.
- All failure-scenario tests in §3 pass.

**Estimated effort:** 1 week (after M14).

### M16: Workload integration SDK + first-party integration skeletons

**Goal:** Implement the workload integration SDK and the five
first-party integration crates as skeletons (lib.rs + Cargo.toml +
the `WorkloadIntegration` trait impl with empty bodies).

**Deliverables:**
- `zim/engine/crates/workloads/sdk/` with the `WorkloadIntegration`
  trait, the capability trait definitions, the manifest schema, and
  the proc macros for `#[workload_integration]`.
- `zim/engine/crates/workloads/minecraft/` skeleton.
- `zim/engine/crates/workloads/database/` skeleton.
- `zim/engine/crates/workloads/website/` skeleton.
- `zim/engine/crates/workloads/dev_process/` skeleton.
- `zim/engine/crates/workloads/generic_container/` skeleton.
- A workspace-level feature flag (`--features workloads-all`) that
  builds all five integrations.

**Acceptance criteria:**
- Each integration crate compiles with `cargo build -p
  zim_engine_workloads_{minecraft,database,website,dev_process,
  generic_container}`.
- The SDK crate's `WorkloadIntegration` trait is documented with
  doc-comments; an example integration in `sdk/examples/` shows how
  to implement it.
- `cargo test -p zim_engine_workloads_sdk` passes (the SDK has
  unit tests for the manifest parser and the capability set type).

**Estimated effort:** 2-3 weeks.

### M17: Minecraft integration parity with Pterodactyl eggs

**Goal:** Implement the Minecraft workload integration to the point
where it can run a real Minecraft server from an imported egg.

**Deliverables:**
- Full `GameServerSpec` parsing from Pterodactyl egg JSON.
- `ConsoleCapable` implementation (attach console stream, send
  input).
- `FilesystemCapable` implementation (list, read, write, delete,
  archive, extract — the original Wings file API reproduced).
- `ExecCapable` implementation (run install scripts in a
  constrained container).
- `MetricsCapable` implementation (resource usage via Docker stats).
- `Migratable` implementation (file-copy migration; `migration_plan`
  returns `requires_quiesce: true` for safety).
- Integration tests against a real Minecraft server container.

**Acceptance criteria:**
- An imported Minecraft egg can be installed and started via the
  ZIM API.
- The console stream shows real Minecraft server output.
- Stop sends `stop` to the console (Minecraft convention) and waits
  for the process to exit.
- A backup can be created and restored.

**Estimated effort:** 4-6 weeks.

### M18: Database integration (Postgres + MySQL/MariaDB + Redis)

**Goal:** Implement the database workload integration for the
three most common database engines.

**Deliverables:**
- `DatabaseSpec` parsing.
- `ReadyProbeCapable` implementation (per-engine readiness probe:
  `pg_isready`, `mysqladmin ping`, `redis-cli ping`).
- `SnapshotCapable` implementation (Docker volume snapshot;
  database quiesced via `CHECKPOINT` / `FLUSH TABLES WITH READ LOCK`
  / `BGSAVE` before snapshot).
- `NetworkCapable` implementation (multiple endpoints per
  database: read/write/replication).
- `Migratable` implementation with a multi-step plan: quiesce,
  checkpoint WAL, snapshot, ship snapshot, restore, catch up.
- Integration tests against real Postgres, MySQL/MariaDB, Redis
  containers.

**Acceptance criteria:**
- A Postgres workload can be installed, started, queried via a
  SQL client, snapshotted, and restored.
- The readiness probe correctly reports `NotReady` during recovery
  and `Ready` after.
- A migration to a second node succeeds with documented downtime.

**Estimated effort:** 4-6 weeks.

### M19: Website + DevProcess + GenericContainer integrations

**Goal:** Implement the remaining three first-party workload
integrations.

**Deliverables:**
- Website integration: `WebsiteSpec` parsing, `HealthCheckCapable`
  implementation (HTTP/TCP health checks), `NetworkCapable`
  (HTTP/HTTPS/admin ports), deploy lifecycle (new version deployed
  alongside old, drain old, switch).
- DevProcess integration: `DevProcessSpec` parsing, file-watch
  restart via `notify` crate, no install/backup expectations.
- GenericContainer integration: `GenericContainerSpec` parsing,
  capability negotiation at placement time (whatever the spec
  declares is what the runtime must provide).
- Integration tests for each.

**Acceptance criteria:**
- A static website can be deployed and served by ZIM.
- A `cargo watch` dev process can be supervised with file-watch
  restart.
- A generic container can be placed on a runtime that provides
  the capabilities it declares.

**Estimated effort:** 3-4 weeks.

## 4. Out-of-scope for the first release

The following are explicitly out-of-scope for the first ZIM
release and are deferred to later milestones:

- Legacy client API compatibility (`/api/legacy/client/v1/...`).
- Legacy application API compatibility.
- Managed databases (the engine stores the rows but no
  database-management UI in the first release).
- SFTP server.
- ZIM addon marketplace (addons can be installed manually via
  upload; no central marketplace).
- Telemetry (the module is specified; the actual telemetry
  ingestion is deferred).
- Mobile companion app.

## 5. Sequencing rationale

The milestones are sequenced to respect dependencies:

- M1 (toolchain) unblocks all engine work.
- M2 (skeleton) gives the workspace structure.
- M3 (domain + persistence) is the foundation; everything else
  uses domain types.
- M4 (auth + authz) is needed before any privileged operation.
- M5 (API) is the surface; M6-M9 (runtime, streaming, jobs,
  eggs+scheduler) provide the operations the API exposes.
- M10 (migration) can run in parallel with M6-M9 (independent of
  runtime).
- M11 (frontend) needs the API and streaming; ships a minimal
  surface.
- M12 (Chromium import) is gated on toolchain + disk + memory;
  can run in parallel with M3-M11.
- M13 (integration) needs M11 (frontend) and M12 (Chromium
  build).
- M14 (E2E + packaging) needs M13 (a built browser).
- M15 (release) needs everything.

Critical path: M1 → M2 → M3 → M5 → M11 → M13 → M14 → M15.
Parallelizable: M4 (with M3), M6/M7/M8/M9 (after M5), M10 (after
M3), M12 (after M1+environment).

## 6. First Rust implementation milestone

Per the prompt's requirement that the roadmap identify "the first
Rust implementation milestone, its interfaces, its tests and its
completion criteria":

**First Rust implementation milestone: M2 — Rust engine skeleton.**

Interfaces:
- `zim/engine/Cargo.toml` workspace manifest.
- One crate per proposed module in
  `docs/architecture/RUST_ENGINE.md` §1.
- Each crate's `lib.rs` declares its public types/traits (empty
  bodies are acceptable at this milestone; the goal is to
  establish the dependency graph).

Tests:
- `cargo build --workspace` succeeds.
- `cargo test --workspace` succeeds (no tests; no compile errors).
- `cargo clippy --workspace -- -D warnings` succeeds.
- `cargo fmt --check` succeeds.
- CI workflow runs all four on every push.

Completion criteria:
- The workspace compiles.
- The crate graph enforces the permitted dependency direction
  from `docs/architecture/RUST_ENGINE.md` §2 (verified by
  attempted `use` of a forbidden type failing to compile).
- CI is green on `main`.

## 7. Risks and contingencies

- **Risk:** The Rust workspace's proposed crate split turns out
  to be too granular (real dependency graph is tighter). Mitigation:
  consolidate crates during M2; the permitted dependency direction
  is preserved regardless of crate count.
- **Risk:** The Chromium build takes much longer than expected
  in the build environment. Mitigation: M12 is parallelizable with
  engine work; a delay in M12 doesn't block M3-M11.
- **Risk:** The Docker runtime's bollard client has undocumented
  behavior that affects the engine's invariants. Mitigation: M6
  includes a thorough integration test suite; bollard
  idiosyncrasies are surfaced and worked around.
- **Risk:** The migration importer encounters Pterodactyl Panel
  databases with edge-case data (nulls, oversized fields,
  non-JSON-serializable metadata). Mitigation: M10 includes a
  fixture database with edge cases; the importer's behavior is
  tested against each.
- **Risk:** The default frontend's chosen framework (vanilla TS +
  Web Components) turns out to be insufficient for the UI's
  complexity. Mitigation: the framework is not a dependency of the
  engine; the frontend can be rewritten without affecting the
  engine. M11 ships a minimal surface; richer UI is a later
  milestone.
