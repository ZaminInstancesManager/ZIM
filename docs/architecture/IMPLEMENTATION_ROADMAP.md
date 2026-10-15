# Implementation Roadmap (Phase 6)

> **CORRECTION (2026-10-15, AD-20):** Milestones M12-M15 (Chromium
> source import, C++ integration layer, browser-gated E2E /
> packaging / release) are **cancelled** — ZIM is a Chrome-inspired
> desktop application, not a Chromium fork. The corrected milestones
> M20-M26 below supersede them. M1-M11 and M16-M19 (engine +
> frontend package) are unaffected and complete. Blockers B2-B4
> (depot_tools / Chromium disk / Chromium link memory) are moot.

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

### M12-M15: CANCELLED — Chromium integration (superseded by AD-20)

The four Chromium milestones — source import + patch queue (M12),
the C++ integration layer (M13), E2E + packaging gated on a built
browser (M14), and the first release (M15) — are cancelled as of
2026-10-15. ZIM is a Chrome-inspired desktop application, not a
Chromium fork: no Chromium source is imported, built, patched, or
shipped, and no 100 GB build environment is ever required. The
E2E / packaging / release scope is re-scoped into M25/M26 below.
The original acceptance criteria of M12-M15 are void; M20-M26 are
the corrected continuation of this roadmap.

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

### M20: Architecture correction — COMPLETE (2026-10-15)

**Goal:** Correct the product direction across the architecture
documentation and plan, preserving all valid engine work.

**Deliverables:**
- AD-20/AD-21/AD-22 recorded; AD-2/AD-7/AD-9 superseded;
  Q1-Q3 resolved as moot.
- `docs/architecture/BROWSER_INTEGRATION.md` rewritten as the
  desktop-frontend architecture (Chrome-UX reference inventory,
  shell architecture, preserved URL inventory / status model /
  session invariants).
- This roadmap's Chromium milestones cancelled and replaced with
  M21-M26.
- All affected architecture/audit/API/security docs carry
  correction banners; `chromium/UPSTREAM_MANIFEST.md` is marked
  cancelled (provenance only).

**Acceptance criteria:** No doc instructs anyone to import, build,
patch, or require Chromium; the engine milestones M1-M19 remain
described as complete and valid; the corrected path forward
(M21-M26) is fully specified.

**Status:** Complete.

### M21: Desktop shell scaffold (Tauri 2) — COMPLETE (2026-10-15)

**Goal:** A thin desktop shell hosting the engine-served frontend.

**Deliverables:**
- `zim/shell/`: the Tauri 2 application (window, OS integration,
  tray, app chrome skeleton).
- `zim://` resolution onto the engine's frontend loader — custom
  protocol vs. loopback-origin mapping (AD-22 spike; pick and
  record in BROWSER_INTEGRATION.md §13).
- Engine lifecycle independence: closing/crashing the shell never
  stops the engine; the shell attaches to an already-running engine
  and starts it when absent.
- CI workflow building the shell on Linux, Windows, and macOS
  runners.

**Acceptance criteria:**
- `zim://newtab` renders the engine-served new-tab page inside the
  shell.
- With a server running, killing the shell leaves the server
  running; relaunching the shell reattaches and shows live state.
- Shell CI green on all three platforms.

**What shipped:**
- `zim/shell/` (Tauri 2, its own cargo workspace so the engine tree
  stays GUI-dependency-free): `cli` (flag surface), `zim_url`
  (AD-23 mapping + the full §3 inventory as tests), `engine`
  (discovery-file read, loopback-TLS health probe, detached spawn,
  the composed attach sequence), `trust` (OS trust-store
  installation of the engine's persistent loopback certificate),
  `tray` (show / new-tab / "engine keeps running" quit), bundled
  `ui/boot.html` + `ui/down.html` pre-engine pages, tray and window
  wiring in `lib.rs`.
- Engine side (additive, small): the `engine.json` discovery file —
  written at boot, removed on graceful shutdown
  (`zim/engine/crates/main/src/discovery.rs`), smoke-tested.
- AD-23 recorded (resolution mechanism, discovery contract, trust
  strategy); BROWSER_INTEGRATION.md §13 Q2 resolved with M21
  resolution notes; this roadmap updated.
- CI: `Shell CI` — a 3-platform build/clippy/fmt/test matrix plus a
  Linux `engine-integration` job that runs the shell's REAL attach
  logic (discovery → probe → page fetch over real TLS → crash →
  respawn → graceful-shutdown discovery removal) against a real
  `zim_engine` binary. Engine CI's push trigger additionally covers
  `m*-dev` branches (the dev-loop pattern, generalized).

**Acceptance status (honest):**
- `zim://newtab` rendering: implemented end to end
  (discovery/probe/trust/navigation); CI verifies every layer it
  can without a display session (the integration job's page fetch
  IS the navigation target, over real TLS). The literal webview
  pixel-render needs an interactive desktop session — first manual
  acceptance pass pending, expected during M22 bring-up.
- Engine lifecycle independence: structural (detached process
  group / detached console spawn; no kill handles anywhere; the
  tray quit says "the engine and servers keep running"); exercised
  by the integration test's crash/respawn sequence.
- Shell CI green on all three platforms: the acceptance gate of
  this milestone's dev loop (see the workflow runs on the merge
  commit).
- Auth UX (login into a shell-spawned engine) is M22 scope; the
  NTP renders unauthenticated, exactly as on any browser today.

**Estimated effort:** 1-2 weeks.

### M22: Chrome-UX parity I — tab strip, address bar, navigation — COMPLETE (2026-10-15)

**Goal:** Replicate the Chrome tab-strip and omnibox interaction
model in the frontend's own TypeScript.

**Deliverables:**
- Tab strip: one tab per server/workload view; drag-reorder,
  close, tear-off-to-window, overflow, Chrome animation curves.
- Per-tab workload status indicator wired to
  `instance.state_change` over the stream (the §4 status model →
  visual mapping).
- Address bar for `zim://` navigation with suggestion dropdown and
  engine-identity indication.
- Tab context menu with the §5 items, permission-gated.
- Session persistence of tab sets as `zim://` URLs; restore
  restores views only, never lifecycle.

**Acceptance criteria:**
- The UX-parity checklist (tab strip + omnibox sections) passes.
- Indicators track live engine state in real time over WSS.
- Session restore re-opens views with zero lifecycle side effects.

**What shipped:**
- `zim://app` — the application chrome (AD-24): a declared frontend
  surface (`pages/app.html` + `src/app.ts`) hosting tab panes as
  same-origin iframes. Works in any browser, not just the shell.
- Tab strip (`src/app.ts` + `src/tabs.ts`): click/middle-click
  close, Enter/Space and arrow-key keyboard operation, pointer-drag
  reorder with FLIP glide and sibling shifting, drag-down tear-off,
  open/close animations on Chrome's curve
  (`cubic-bezier(0.2, 0, 0, 1)` @ 167 ms), tabs flex to the 54 px
  Chrome minimum with chevron + wheel overflow, new-tab button at
  the strip's right, and the one-tab-stays-open policy.
- Per-tab indicators: the §4 mapping fully realized in
  `<zim-status-dot>` (pulsing transient states, conic progress ring
  from `instance.install_progress`, crash-loop glyph, small
  favicon-slot variant); the chrome holds one §11 events stream per
  server tab — `instance.state_change` updates the dot in real
  time, gaps re-fetch over HTTP, fatal streams degrade to one HTTP
  fallback. Engine's `visual_color` aligned: Suspended → gray.
- Omnibox (`src/omnibox.ts`): canonical `zim://` display form
  (AD-23's obligation), `zim://`-fixed scheme prefix, select-on-
  focus, suggestion dropdown (address-first ranking, §3 surfaces,
  instance consoles, error rows for unsafe input), keyboard
  navigation, and the engine-identity chip (health version, port,
  signed-in user) where Chrome shows site identity.
- Tab context menu (§5): Open console/files/settings/backups/
  metrics, Start/Stop/Restart state-gated with the engine's
  structured permission errors surfaced (missing grants listed),
  Copy server URL, Reload, Duplicate; "Open in split view" is
  present-but-disabled and honestly labeled (M23).
- Session persistence (§7): `zim.session.v1` = `{v, tabs, active}`
  of `zim://` paths, debounced writes + unload flush, corruption
  rejects to the default session, `?tab=` deep links (tear-off
  targets) replace the session; restoration re-opens views only.
- The six per-instance §3 surfaces declared and served (one shared
  stub page, honest per-surface notes; the metrics surface renders
  real recent samples from `GET /instances/{id}/metrics`).
- Auth UX: the login form (new-tab page inside a pane) relays
  `zim:authenticated`/`zim:signed-out` to the chrome via
  same-origin postMessage; the chip, suggestions, and instance
  titles refresh. Tab tokens stay unissued (session cookie is not
  tab-scoped; per-tab counting waits for M24 extension panes —
  engine `stream.rs` documents the reasoning).
- Shell: default target `zim://app`; tray "New tab — zim://app"
  navigates to `/zim/app?newtab=1` (the chrome restores the session
  plus one new tab; the identifier grammar has no query strings, so
  the shell builds the HTTP form directly).
- Engine: page CSP gains `frame-src 'self'`/`frame-ancestors
  'self'` with `X-Frame-Options: SAMEORIGIN` (AD-24's framing
  posture); page tests cover the new directives and surfaces.
- Fixed on the way: the compiled assets' extensionless import
  specifiers never resolved in real ES-module hosts — the build now
  post-processes specifiers (`scripts/fix-imports.mjs`) and CI's
  freshness gate catches drift.
- Tests: 33 frontend unit tests (node:test — the zim:// grammar,
  tab model + session format, omnibox ranking; `npm test` in the
  CI frontend job), engine page-test additions (CSP, `/zim/app`,
  stub surface), shell `zim_url` inventory row + the attach
  integration fetching `/zim/app` over real TLS.

**Acceptance status (honest):**
- UX-parity checklist (tab strip + omnibox rows of §12): the
  interaction model is implemented and unit-tested where the logic
  is pure; visual/animation fidelity is authored to Chrome's
  documented motion values but needs a human pass in the packaged
  shell (expected during M23 bring-up, like M21's pending render
  pass).
- Indicators over WSS: implemented end to end; the stream
  client's reconnect/gap/fatal paths are exercised engine-side, and
  the integration test fetches the chrome page over real TLS.
- Session restore with zero lifecycle side effects: enforced by
  construction (pages are view-only; the chrome issues reads
  only) and covered by the session-format unit tests.
- Tear-off: works where `window.open` works (plain browsers);
  inside the desktop shell it reports that multi-window arrives
  with M23 (which owns tear-out/merge natively) — the §12 row is
  deliberately split across M22/M23 rather than faked.

**Estimated effort:** 2-3 weeks.

### M23: Chrome-UX parity II — split view, windows, themes, NTP

**Status: COMPLETE** (frontend `src/split.ts` + `src/theme.ts` +
`src/instanceForm.ts` + the `tabs.ts` v2 session format + the
rewritten NTP + the `app.ts` split/multi-window chrome; engine
`/me/recents` + migration 0005 + the IPC CSP sources; shell
window registry + tear-off/merge commands + in-place new-tab).

**What shipped:**

- Split view (§8): one split per tab — the primary pane plus a
  secondary pane, each its own iframe and its own §11 stream
  subscription. The divider drags (pointer events) with Chrome's
  snap points (halves and thirds, 3% tolerance, a visible snap
  state), double-click re-centers, arrow keys nudge, and a close
  affordance on the divider closes only the secondary pane —
  `stream.close()` on that pane alone; the primary pane and the
  server are untouched. "Open in split view" (§5 menu) copies the
  tab, Chrome-style; the split pairing serializes into the session
  (v2) with a clamped ratio, and a torn-out split tab carries both
  paths (`?tab=` + `?split=` + `?ratio=`).
- Session v2 (`tabs.ts`): entries are `{path, split?}`; the v1 key
  is still read once for upgrade; corruption voids the whole
  session (a bad split path is corruption, a drifted ratio is
  clamped); ≤64 tabs stands. Per-window keys: the main window owns
  `zim.session.v2`; a torn-off window (`?window=chrome-N`) owns
  `zim.session.v2.w:chrome-N` — windows never clobber each other's
  sessions on the shared origin (hostile labels fall back to the
  primary key).
- Multi-window + tear-out/merge (AD-26): inside the shell, tear-off
  invokes `zim_tear_off` — the shell resolves every path through
  the `zim://` grammar (AD-23 holds), builds the window, registers
  its label, and the chrome closes the torn-out tab. A torn-off
  window's tab menu offers "Merge back to the main window":
  `zim_merge_to_main` relays the whole v2 session as a
  `zim:merge-session` event (the main chrome validates it with the
  same restore discipline) and closes the window. The shell
  persists the window set (`windows.json`, corrupt = empty) and
  recreates remembered windows after attach — each restores its own
  session from its own storage key; no tab state crosses IPC. The
  tray gained "New window — zim://app", and its "New tab" is now an
  in-place `zim:new-tab` event (no reload; the `?newtab=1`
  navigation remains the fallback). Plain browsers keep M22's
  `window.open` tear-off and simply never see merge.
- Themes (§12 row, AD-27): five presets (ZIM Dark, Midnight,
  Forest, Ember, Paper — one light) + an accent pip, applied live
  through CSSOM custom-property writes on `:root` (CSP-clean under
  `style-src 'self'`; CSSOM writes are not markup style
  attributes). The shadow-root component styles were re-tokenized
  to `var(--…, fallback)` so custom properties inherit through the
  shadow boundaries. The token vocabulary contract ships as
  `zim/contracts/theme-tokens.v1.json`; packaged-theme upload
  (`POST /api/v1/themes`) stays with M24's addon work.
- NTP completion (§9): centered search/filter over the caller's
  servers, a Recent tile section (engine-backed `GET /me/recents`,
  §11 — not a browser-history database; frequent tiles badge their
  open counts), an All-servers grid with live status dots, create
  and import actions (admin-gated honestly — non-admins see the
  sign-in state, a 403 on node listing says so), workload-type
  browsing (game servers live; other kinds honestly marked "later"
  until M16), and the customization entry point (the theme panel).
  The create dialog posts the engine's `POST /instances` shape
  (name + node + optional fields); import parses a pasted JSON
  server definition through the same validation (pure,
  unit-tested) and points panel migrations at the
  `zim-migrate-pterodactyl` CLI. Successful create/import opens
  the new server's console — the §9 end-to-end flow.
- Engine: `GET/POST /api/v1/me/recents` (user-scoped, `limit`
  clamped `[1,32]`, 404 on unknown instances, not audited — an
  open is nicety state, not an authority move), migration 0005
  (`user_recents`, capped at the newest 32 per user on write),
  domain `RecentServer`, port methods, sqlite store. The page CSP
  gains `ipc:` + `http://ipc.localhost` in `connect-src` (AD-26:
  the chrome's shell-command channel; no network reach — the
  schemes terminate in the shell process).
- The chrome records pane-open adoptions fire-and-forget
  (`POST /me/recents`, only while signed in, once per adoption).

**Acceptance status (honest):**
- Split panes subscribe independently; closing one pane
  unsubscribes only that pane; the server keeps running — by
  construction (each pane owns its stream; `closeSplitSecondary`
  never touches the primary pane's stream) and covered by the
  split/session unit tests.
- Themes apply live — CSSOM token writes on every page's boot +
  the NTP panel; covered by the theme unit tests (resolution,
  persistence validation, pip rules). Visual fidelity of the
  presets needs a human pass in the packaged shell (as with every
  visual row).
- NTP flows create/open a server end-to-end — the create dialog +
  the engine's instance endpoint are integration-tested
  (`recents_record_rank_and_isolate_per_user` walks login → node →
  instances → recents over real TLS); the page-level flow needs
  M25's E2E to be pressed in the packaged app.
- Multi-window: window creation/restore/registry is unit-tested
  shell-side; the full OS-window flows (tear-off drag, merge,
  per-window storage across restarts) need the packaged shell —
  the same honest boundary M21/M22 recorded for window work.
- Split-all / drag-between-windows merge: not shipped (Chrome's
  split-all and cross-window drag-merge are polish behind the
  milestone's acceptance line; the merge path that exists is the
  menu-driven one).

**Estimated effort:** 2-3 weeks.

### M24: Extension / addon system — COMPLETE (2026-10-15)

**Status: COMPLETE** (engine `zim_engine_extensions` — manifest,
package, signature, service, theme; the §10.12 handler family +
the `/zim/` addon fallback routing + `POST/GET/DELETE
/api/v1/themes` in `zim_engine_api`; the addon SDK
`zim/sdk/addon/` + `docs/api/ADDON_SDK.md`; the bundled
`org.zaminmc.extension.audit-log` + the sample `hello-zim`; the
frontend's `zim://extensions` management page + chrome
contributions; AD-29).

**What shipped:**

- Engine-side addon loading per `FRONTEND_EXTENSION_SPEC.md` §4:
  the `zim.addon.json` manifest (§4.1 — full structural validation:
  the closed permission vocabulary, instance scope, surface
  well-formedness + entrypoint routing, sidebar/context-menu
  cross-references, command accelerators with §4.12's reserved
  set rejected, the closed event vocabulary, `id@range`
  dependencies, the optional §3.4-signature block), the
  `.zimpkg` ZIP format (confinement + 32 MiB / 1000-file caps),
  and the signature classification shared with frontend packages
  (one trust root, one block shape; `invalid` is a hard reject).
- The lifecycle (§4.11): install (admin upload IS the approval;
  generation/engine-range gating; §4.9 dependency resolution with
  no partial installs; §4.4 surface-collision refusal incl. the
  reserved `zim://ext/` namespace), enable/disable, update
  (strictly-greater versions; downgrades refused), uninstall
  (dependents block; §6's bundled defaults are disable-only,
  DELETE → 403), and grant (§4.2's admin re-approval for
  elevation). Every lifecycle move is audited. Durable state in
  the M3-era `extension_registrations` rows; extracted package
  trees under `{data_dir}/extensions/packages/{id}/{version}/`.
- Error isolation (§4.10, adapted per AD-29): a package whose
  on-disk manifest/tree fails to load at boot is disabled with a
  notice surfaced at `zim://extensions`; the engine, every other
  addon, and the frontend keep working. The bundled defaults
  repair missing trees from build-time-embedded bytes.
- Serving: enabled addons' declared surfaces serve through the
  `/zim/` router's fallback with the same §3.8 CSP, nosniff,
  referrer, and digest-ETag discipline; their own files serve
  under the reserved `zim://ext/{addon-id}/…` namespace (AD-29's
  honest analog of §4.10's per-origin URLs).
- The documented addon SDK (`zim/sdk/addon/`, `dist/zim.js`
  vendored into packages; `docs/api/ADDON_SDK.md` is the author's
  contract): `Zim.init` (resolve the registration),
  `Zim.api` (§6-shaped errors + the client-side permission gate),
  `Zim.ZimStream` (one §11 subscription per instance),
  `Zim.whoami`.
- Manual install via upload (no marketplace — §4): the
  `package-addon.mjs` helper (validate + build with SDK vendoring)
  and the admin upload path end-to-end.
- The bundled default `org.zaminmc.extension.audit-log` (§6): a
  real audit-log viewer (cursor-paginated `GET /api/v1/audit`,
  honest 403 state for non-admins, §4.5 sidebar + §4.7 command),
  embedded at build time, seeded on fresh installs,
  disable-only. The other two §6 defaults (databases,
  recycle-bin) are deferred with the roadmap's §4 out-of-scope
  line (managed-database UI; the file API has no recycle
  semantics yet) — the addon system that carries them is now
  real. The sample `zim/extensions/samples/hello-zim/`
  demonstrates every manifest capability.
- The frontend: `zim://extensions` (the management page — list
  with contributions/notices/signature classes, admin
  enable/disable/uninstall/grant/upload), §4.6 context-menu
  items in the app chrome, §4.5 sidebar items on the console
  page, §4.7 command accelerators (navigating the active tab to
  the addon's surface), and the theme panel's uploaded-engine
  themes group.
- Packaged themes (M23's deferral): `POST/GET/DELETE
  /api/v1/themes` — `.zip` with `zim.theme.json` at the root,
  tokens validated against `theme-tokens.v1.json` (closed
  vocabulary, literal values, size caps), stored in the data dir,
  chosen from the NTP panel with a token snapshot persisted so
  every page's synchronous boot applies them (the scheme inferred
  from the background's luminance).

**Acceptance status (honest):**

- Manifest/package/lifecycle/serving are covered by unit tests
  (the extensions crate) and socket-level integration tests
  (`api_surface.rs`: install → surface serves → ext/ namespace
  serves → 409 collisions (frontend's own surface, same-version)
  → disable stops serving → enable resumes → grant accumulates →
  bundled DELETE → 403 → uninstall → 404 → non-admin → 403; the
  theme family's upload/list/bad-token/delete round trip).
- §4.10's isolation: disable-with-notice is tested (corrupt the
  extracted manifest → rebuild → disabled + notice, others
  unaffected). The spec's *process* isolation is retired by AD-29,
  honestly, not simulated.
- §4.2's engine-boundary enforcement: per-endpoint authz gates
  every call (invariant 15, unchanged); per-addon attribution on
  the shared loopback origin is an M25 hardening item — recorded,
  not faked.
- Visual fidelity of the audit-log page and the extensions page
  needs a human pass in the packaged shell (as with every visual
  row); the flows themselves are engine-tested.
- §6's full bundled set: one of three shipped (audit-log); the
  other two deferred with their owning out-of-scope lines.

**Estimated effort:** 2-3 weeks.

### M25: E2E suite + cross-platform packaging

**Deliverables:**
- E2E suite covering the TEST_STRATEGY scenarios: install ZIM,
  create an instance, start it, view its console, stop it — on
  Windows, Linux, macOS.
- Installers per platform + auto-update channel mechanics.
- Cross-platform CI matrix for the packaged app.

**Estimated effort:** 3-4 weeks.

### M26: First usable ZIM release (alpha)

**Deliverables:**
- Release binaries for Windows x64, Linux x64, macOS arm64.
- Release notes (what works / what doesn't) and the Pterodactyl
  migration guide.
- Alpha labeling: not production-ready.

**Acceptance criteria:** M1-M11, M16-M25 complete; the
architectural-invariant and failure-scenario suites pass.

**Estimated effort:** 1 week (after M25).


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
- M12-M15 (Chromium integration) are cancelled (AD-20).
- M21 (shell scaffold) needs M11 (engine-served frontend).
- M22/M23 (Chrome-UX parity) need M21.
- M24 (addons) can run in parallel with M22/M23.
- M25 (E2E + packaging) needs M22/M23.
- M26 (release) needs everything.

Critical path: M1 → M2 → M3 → M5 → M11 → M21 → M22 → M25 → M26.
Parallelizable: M4 (with M3), M6/M7/M8/M9 (after M5), M10 (after
M3), M24 (with M22/M23).

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
