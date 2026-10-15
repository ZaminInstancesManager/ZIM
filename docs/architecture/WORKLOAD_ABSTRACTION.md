# Workload Abstraction (Phase 6 expansion)

> **CORRECTION (2026-10-15, AD-20):** Layer-1 ("Browser platform")
> is re-scoped: the desktop shell (Tauri 2, system WebView) +
> engine-served `zim://` pages own windows/tabs/navigation/session;
> there are no Chromium local patches. The layer model and
> capability traits are otherwise unchanged.

This document is canonical for ZIM's general workload model. It
generalizes the narrower "managed game server" concept in the
original architecture documents into a generic Workload abstraction
that can describe game servers, databases, websites, development
processes, containerized applications, and future workload types
introduced by first-party modules or third-party extensions.

It is additive to the existing architecture documents. Where this
document and another document disagree about workload terminology,
**this document is canonical** for the general model; the other
document is canonical for the specific concern it addresses (e.g.
`docs/architecture/RUST_ENGINE.md` remains canonical for Rust crate
structure; `docs/api/ZIM_API_SPEC.md` remains canonical for the
HTTP surface).

## 1. Why a Workload abstraction

The original ZIM architecture (in `docs/architecture/RUST_ENGINE.md`
§3) modeled every managed thing as an `Instance`, with a single
startup command, a single Docker image, an `allocation_id` (one
primary port), a `startup: CommandTemplate`, and an `image:
DockerImage`. That shape is a faithful generalization of
Pterodactyl's `Server` model and is correct for the workload class
Pterodactyl was designed for (game servers, especially Minecraft).

The expanded product vision makes that shape too narrow. A database
workload:

- Does not have a "console" that accepts free-text commands in the
  same sense a Minecraft server does. It has a SQL interface exposed
  over a network port, and lifecycle commands like `pg_ctl start`.
- Has a *readiness probe* (the DB accepts connections) that is
  distinct from "the process is running" (the postmaster is up but
  the DB is still recovering).
- Has *initialization* behavior (run `initdb`, apply migrations,
  create roles) that is meaningfully different from "run an install
  script in a one-shot container".
- Cannot be safely stopped by sending `SIGTERM` and hoping; it may
  need to wait for in-flight transactions.
- Cannot be migrated by simply copying its data directory while
  running; it must be quiesced first.
- Has multiple network endpoints (read port, write port, replication
  port, admin port), not a single allocation.

A website workload:

- May have a health-check endpoint rather than a console.
- May have multiple processes (web server + background worker +
  asset compiler).
- May have a "deploy" lifecycle rather than a "start" lifecycle.
- May not have a Docker image at all (it might be a static directory
  served by a ZIM-managed web server).

A long-running development process:

- May be a `cargo watch` or `npm run dev` process that the user
  wants supervised but not "installed" or "backed up" in the
  game-server sense.
- May have multiple "tabs" worth of state (the build log, the test
  runner, the running server preview).
- May be stopped and restarted frequently by the user, with crash
  detection being undesirable (the user killed it on purpose).

A generic containerized application:

- May have all of the above variation; the engine should not assume
  any specific shape.

ZIM therefore introduces a `Workload` abstraction that:

- Has a `kind: WorkloadKind` discriminator identifying what kind
  of workload it is.
- Has a `spec: WorkloadSpec` carrying kind-specific configuration
  (a typed enum, not a string bag).
- Owns an `Instance` (the existing type from
  `docs/architecture/RUST_ENGINE.md` §3) which carries the
  Pterodactyl-compatible fields used for migration compatibility.
- Has a `Runtime` that may implement zero or more *capability traits*
  (`ConsoleCapable`, `HealthCheckCapable`, `ReadyProbeCapable`,
  `FilesystemCapable`, `NetworkCapable`, `SnapshotCapable`, etc.).
- Reports *unsupported operations explicitly*; an operation the
  workload's runtime does not support returns
  `Unsupported { reason }`, not a silent no-op or a fake success.

## 2. The five logical layers (per the expanded scope §3)

ZIM's architecture is organized into five logical ownership layers.
These are *logical* boundaries; they are not five separate OS
processes and not five separate Rust crates. The boundary is
enforced by module visibility and trait ownership, not by process
isolation (except where the layer is also a process, e.g. the
browser and engine are separate processes).

| Layer                              | Responsibility                                                                  | Implementation owner                                              | Process model |
|-------------------------------------|----------------------------------------------------------------------------------|-------------------------------------------------------------------|---------------|
| 1. Browser platform                 | Chromium windows, tabs, navigation, rendering, session restore, sandbox, extensions, web compat. | Chromium upstream + ZIM local patches under `chromium/PATCHES/`. | Browser process (Chromium-derived). |
| 2. ZIM user experience              | Workload tabs, settings, internal pages, themes, frontend packages, user-facing workflows. | `zim/frontend/` (default) + `zim/extensions/` (bundled addons) + `zim/integration/` (browser-side C++ that wires WebUI to the engine). | Renderer processes (one per `zim://` origin) + the browser process. |
| 3. ZIM engine                       | Authoritative state, authorization, scheduling, persistence, lifecycle orchestration, event delivery, jobs, auditing, recovery. | `zim/engine/` Rust workspace. | Engine process (Rust; independent of the browser). |
| 4. Runtime adapters                  | Interfaces to OS processes, containers, remote agents, workload-specific execution environments. | `zim/engine/crates/runtime-*` and `zim/engine/crates/node-runtime/`. | In the engine process (local) or in the remote-node process (remote). |
| 5. Workload integrations              | Minecraft management, generic application management, databases, websites, future integrations. | `zim/engine/crates/workloads/{minecraft, database, website, dev_process, generic_container}` and (future) third-party workload packages. | Loaded into the engine process (in-process) for first-party; isolated by capability grant, not by process. |

### Boundary rules

- **Browser ↔ Engine:** the browser is never authoritative for
  workload state. Browser crashes do not affect the engine.
  Renderer crashes do not affect the engine. Closing all browser
  windows does not stop the engine or any workload.
- **Engine ↔ Runtime adapters:** the engine defines the `Runtime`
  trait (and capability traits); adapters implement them. The
  engine does not call OS-specific APIs directly; it goes through
  the adapter.
- **Runtime adapters ↔ Workload integrations:** a workload
  integration declares what capabilities it requires (e.g. "this
  database integration requires `SnapshotCapable` and
  `NetworkCapable` with multi-port support"); the engine refuses
  to place the workload on a runtime that does not provide the
  required capabilities.
- **Workload integrations ↔ Engine core:** workload integrations
  do not get blanket authority over the engine. They are
  capability-scoped: a Minecraft integration may declare it can
  manage workloads of kind `GameServer`; it cannot manage workloads
  of kind `Database` unless it also declares that capability and
  the admin grants it.

## 3. The Workload domain model

```rust
// In zim_engine_domain::workload

pub struct WorkloadId(pub Uuid);

pub struct Workload {
    pub id: WorkloadId,
    pub instance_id: InstanceId,       // link to the Instance (Pterodactyl-compat fields)
    pub kind: WorkloadKind,
    pub spec: WorkloadSpec,            // typed enum; kind-specific config
    pub runtime_kind: RuntimeKind,      // Container / Host / Remote
    pub capabilities: DeclaredCapabilities,  // what this workload expects its runtime to provide
    pub owner_id: UserId,
    pub node_id: NodeId,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub enum WorkloadKind {
    GameServer,           // Minecraft, Paper, etc. (the original ZIM/Instance shape)
    Database,             // Postgres, MySQL, Redis, etc.
    Website,              // static or dynamic web apps
    DevProcess,           // long-running dev processes (cargo watch, npm run dev)
    GenericContainer,    // arbitrary containerized workload
    Custom(WorkloadKindId),  // extension-provided workload kind
}

pub enum WorkloadSpec {
    GameServer(GameServerSpec),
    Database(DatabaseSpec),
    Website(WebsiteSpec),
    DevProcess(DevProcessSpec),
    GenericContainer(GenericContainerSpec),
    Custom(CustomSpec),  // opaque to the engine; the integration reads it
}

pub struct GameServerSpec {
    pub egg_id: EggId,                  // Pterodactyl egg reference
    pub image: DockerImage,
    pub startup: CommandTemplate,
    pub variables: Vec<VariableBinding>,
    pub allocations: Vec<AllocationId>,  // multiple; the primary one is in Instance.allocation_id
    pub skip_egg_scripts: bool,
    pub crash_detection_enabled: bool,
}

pub struct DatabaseSpec {
    pub engine: DatabaseEngine,         // Postgres / MySQL / Redis / etc.
    pub version: String,
    pub credentials_ref: SecretRef,     // points to the secrets store, never inline
    pub init_script_ref: Option<ScriptRef>,  // SQL or migrations to run on first start
    pub readiness_probe: ReadinessProbe,
    pub backup_strategy: BackupStrategy,  // physical / logical / snapshot
    pub endpoints: Vec<EndpointSpec>,
}

pub enum DatabaseEngine { Postgres, Mysql, Mariadb, Redis, Mongodb, Custom(String) }

pub struct WebsiteSpec {
    pub root: DirectoryRef,             // where the site's files live
    pub server: WebServerSpec,          // nginx-config / static / node / python
    pub health_check: Option<HealthCheckSpec>,
    pub domains: Vec<DomainSpec>,
    pub https: HttpsSpec,
    pub deploy_strategy: DeployStrategy,
}

pub struct DevProcessSpec {
    pub command: CommandTemplate,
    pub working_directory: DirectoryRef,
    pub env: Vec<EnvBinding>,
    pub watch_patterns: Vec<String>,    // files to watch for restart
    pub restart_policy: DevRestartPolicy,  // Never / OnFileChange / Always
}

pub struct GenericContainerSpec {
    pub image: DockerImage,
    pub command: Option<CommandTemplate>,
    pub env: Vec<EnvBinding>,
    pub mounts: Vec<MountSpec>,
    pub ports: Vec<PortSpec>,
    pub health_check: Option<HealthCheckSpec>,
    pub resource_limits: ResourceLimits,
}
```

### Relationship to the existing `Instance` type

`Instance` (from `docs/architecture/RUST_ENGINE.md` §3) remains
the canonical type for Pterodactyl-compat fields (uuid, uuidShort,
external_id, name, description, status, owner_id, memory, swap,
disk, io, cpu, threads, oom_disabled, allocation_id, egg_id,
startup, image, allocation_limit, database_limit, backup_limit,
crash_detection_enabled, installed_at).

For workload kinds where these fields make sense (GameServer,
GenericContainer, DevProcess with resource limits), the `Workload`
references the `Instance` directly. For workload kinds where some
fields don't apply (a static Website doesn't have a `swap` limit;
a Database doesn't have an `egg_id`), the `Instance` is still
created but the inapplicable fields are null/zero in the DB and
the workload integration ignores them.

This preserves the Pterodactyl migration importer's contract
(`docs/compatibility/MIGRATION_AND_COMPATIBILITY.md`) — every
Panel `servers` row maps to an `Instance` row, and a `Workload`
row of kind `GameServer` is auto-created for each.

## 4. Capability traits (the typed adapter capability model)

The `Runtime` base trait (in `docs/architecture/RUST_ENGINE.md`
§4) exposes the *minimum* that any runtime must provide: lifecycle
state queries, start, stop, kill, exec, generation counter, kind
discriminator. The capability traits below are *optional*;
a runtime implements the ones its workload kinds require.

```rust
// In zim_engine_application::supervision::capabilities

/// The runtime accepts text input on a console stream
/// (e.g. Minecraft server console, bash REPL).
/// A runtime that does NOT implement this returns Unsupported
/// when console input is requested.
pub trait ConsoleCapable: Runtime {
    fn attach_console(&self) -> Result<ConsoleStream, RuntimeError>;
    fn send_input(&self, input: &[u8]) -> Result<(), RuntimeError>;
}

/// The runtime exposes a readiness probe distinct from "process is
/// running" (e.g. a database that has started its postmaster but is
/// still recovering; a web server that has bound its port but is
/// still loading routes).
pub trait ReadyProbeCapable: Runtime {
    fn readiness(&self) -> Result<Readiness, RuntimeError>;
    fn wait_until_ready(&self, deadline: Duration) -> Result<(), RuntimeError>;
}

pub enum Readiness { Ready, NotReady { reason: String }, Unknown }

/// The runtime exposes a health-check endpoint (HTTP, TCP, or
/// command) that reports liveness distinct from process presence.
pub trait HealthCheckCapable: Runtime {
    fn health(&self) -> Result<Health, RuntimeError>;
}

pub enum Health { Healthy, Unhealthy { reason: String }, Degraded { reason: String } }

/// The runtime exposes the workload's filesystem for browsing,
/// read, write, archive, extract (the original ZIM file API).
pub trait FilesystemCapable: Runtime {
    fn list_dir(&self, path: &RelativePath) -> Result<DirListing, RuntimeError>;
    fn read_file(&self, path: &RelativePath) -> Result<FileContent, RuntimeError>;
    fn write_file(&self, path: &RelativePath, content: &[u8]) -> Result<(), RuntimeError>;
    fn delete(&self, path: &RelativePath) -> Result<(), RuntimeError>;
    fn archive(&self, paths: &[RelativePath], format: ArchiveFormat) -> Result<ArchiveRef, RuntimeError>;
    fn extract(&self, archive: ArchiveRef, dest: &RelativePath) -> Result<(), RuntimeError>;
}

/// The runtime exposes multiple network endpoints (a database with
/// read/write/replication ports; a website with HTTP/HTTPS/admin
/// ports). Distinct from the single-allocation model in Instance.
pub trait NetworkCapable: Runtime {
    fn endpoints(&self) -> Result<Vec<Endpoint>, RuntimeError>;
    fn expose_port(&self, port: PortSpec) -> Result<Endpoint, RuntimeError>;
    fn unexpose_port(&self, endpoint: &Endpoint) -> Result<(), RuntimeError>;
}

/// The runtime supports snapshot-style backups (e.g. Docker volume
/// snapshots, ZFS snapshots, filesystem-level snapshots) in addition
/// to or instead of file-level archives.
pub trait SnapshotCapable: Runtime {
    fn snapshot(&self, label: &str) -> Result<SnapshotRef, RuntimeError>;
    fn restore_snapshot(&self, snapshot: &SnapshotRef) -> Result<(), RuntimeError>;
}

/// The runtime supports migration (transfer to another node) while
/// the workload is in a particular state. Distinct from "copy the
/// data directory" — a database must be quiesced first.
pub trait Migratable: Runtime {
    fn migration_plan(&self) -> Result<MigrationPlan, RuntimeError>;
    fn prepare_for_migration(&self) -> Result<MigrationCheckpoint, RuntimeError>;
    fn finalize_migration(&self) -> Result<(), RuntimeError>;
    fn abort_migration(&self, checkpoint: MigrationCheckpoint) -> Result<(), RuntimeError>;
}

pub struct MigrationPlan {
    pub requires_quiesce: bool,
    pub estimated_downtime: Duration,
    pub steps: Vec<MigrationStep>,
}

/// The runtime can run an arbitrary one-shot command in the
/// workload's context (used by install scripts, update scripts,
/// admin commands).
pub trait ExecCapable: Runtime {
    fn exec(&self, cmd: ExecCommand) -> Result<ExecOutput, RuntimeError>;
}

/// The runtime exposes resource-usage metrics.
pub trait MetricsCapable: Runtime {
    fn metrics(&self) -> Result<ResourceUsage, RuntimeError>;
    fn metrics_stream(&self) -> Result<MetricsStream, RuntimeError>;
}
```

### Capability discovery and enforcement

At workload placement time, the engine inspects the workload's
`DeclaredCapabilities` and the runtime's implemented trait set
(reported via the runtime's `capabilities()` method). If the
runtime does not implement a trait the workload requires, the
placement fails with `runtime_incompatible`.

At runtime, every capability call is a *dynamic dispatch*:
the engine queries the runtime for the trait object (`runtime
.as_console_capable()`); if `None`, the call returns
`Unsupported { reason: "runtime does not provide ConsoleCapable" }`.

This means **unsupported operations are reported explicitly**,
not silently simulated. A workload integration that asks a
non-ConsoleCapable runtime for console input gets a structured
`Unsupported` error that surfaces to the user; it does not get
a fake success or a silent no-op.

## 5. Workload kind registry and integration packages

Each `WorkloadKind` is registered with the engine via an
integration package. The integration package declares:

```toml
# zim.toml (workload integration manifest)
manifest_version = 1
id = "org.zaminmc.workload.minecraft"
version = "0.1.0"
api_version = "v1"
kind = "GameServer"                      # WorkloadKind variant
display_name = "Minecraft Server"
description = "Minecraft and Minecraft-compatible game servers."
required_capabilities = ["ConsoleCapable", "FilesystemCapable", "MetricsCapable", "ExecCapable"]
optional_capabilities = ["Migratable", "SnapshotCapable"]
permissions = []                          # permissions the integration itself needs (admin-only typically)
config_schema = "schemas/game-server-spec.v1.json"
runtime_kinds = ["Container", "Remote"]   # what runtime kinds this integration supports
signature = "..."
```

The integration package also contains the integration's Rust
implementation (a dynamically loaded library, OR a feature-gated
build-time inclusion for first-party integrations; the
implementation-phase choice is in
`docs/architecture/DECISIONS.md` AD-15).

### First-party workload integrations

Bundled with the engine under
`zim/engine/crates/workloads/{minecraft, database, website, dev_process, generic_container}/`:

- **`minecraft`** — implements `WorkloadKind::GameServer` for
  Minecraft/Paper/Fabric/Forge. Understands: EULA acceptance,
  server JARs, startup flags, game versions, player counts,
  Minecraft console conventions (`/say`, `/stop`, etc.).
  Required capabilities: `ConsoleCapable`, `FilesystemCapable`,
  `MetricsCapable`, `ExecCapable`.
- **`database`** — implements `WorkloadKind::Database` for
  Postgres/MySQL/MariaDB/Redis/MongoDB. Understands: initialization,
  readiness probes, credentials, backup consistency (logical vs.
  physical), connection endpoints. Required capabilities:
  `ReadyProbeCapable`, `FilesystemCapable`, `SnapshotCapable`,
  `NetworkCapable`, `MetricsCapable`.
- **`website`** — implements `WorkloadKind::Website` for static
  and dynamic web apps. Understands: web server config, health
  checks, deploy strategies, HTTPS, domains. Required capabilities:
  `HealthCheckCapable`, `FilesystemCapable`, `NetworkCapable`.
- **`dev_process`** — implements `WorkloadKind::DevProcess` for
  long-running dev processes (cargo watch, npm run dev, etc.).
  Understands: file-watch restart, log streams, no install/backup
  expectations. Required capabilities: `ConsoleCapable`,
  `FilesystemCapable` (optional), `MetricsCapable` (optional).
- **`generic_container`** — implements
  `WorkloadKind::GenericContainer` for arbitrary containerized
  workloads. Required capabilities depend on the spec; the
  integration negotiates capabilities at placement time.

### Third-party workload integrations

Third-party developers can build workload integration packages
that declare a `WorkloadKind::Custom(CustomKindId)`. The engine
loads them via the same integration package manifest mechanism
as ZIM's own integrations; they are subject to capability-grant
review (an integration that declares it manages
`Database`-class workloads requires admin grant).

### Workload integration trust boundary

A workload integration is a Rust library loaded into the engine
process. It is therefore **trusted code** for the engine's memory
safety (Rust memory safety is preserved at compile time; the
integration cannot violate Rust's aliasing rules), but it is
**untrusted for capability scope**: the engine's authz layer
checks every capability call against the integration's declared
scope.

A Minecraft integration that tries to call `DatabaseCapable`-
specific operations on a workload of kind `Database` is rejected
by the engine (the integration's declared scope is `GameServer`).
The integration cannot bypass this by directly accessing the
`Database` integration's memory (Rust's visibility rules enforce
it at compile time; the engine's runtime check enforces it at
call time).

## 6. Lifecycle generalization

The original four-state model (`Offline`, `Starting`, `Running`,
`Stopping`) is preserved as the *base* lifecycle. Each workload
kind may declare an *extended* lifecycle that adds
kind-specific states. Examples:

- **Database:** adds `Recovering` (postmaster is up, DB is in
  recovery), `Initializing` (initdb / migrations running).
- **Website:** adds `Deploying` (a new version is being deployed),
  `Draining` (old version is refusing new connections).
- **DevProcess:** adds `Watching` (file-watcher is active; the
  process may restart on file change).

The extended states are surfaced to the UI as substates of the
four primary statuses (per `docs/architecture/BROWSER_INTEGRATION.md`
§3 status model). E.g.:

- `Recovering` → visual: yellow (Starting), with tooltip "database
  recovering; X% complete".
- `Initializing` → visual: yellow (Starting), with tooltip
  "running migrations".
- `Deploying` → visual: yellow (Starting), with tooltip "deploying
  new version".
- `Watching` → visual: green (Running), with tooltip "watching for
  file changes".

This preserves the four-primary-status model from the original
prompt while honestly representing the broader state space of
non-game-server workloads.

## 7. How this affects the existing Instance model

The `Instance` type from `docs/architecture/RUST_ENGINE.md` §3 is
**not replaced**. It is the Pterodactyl-compatible base record.
A `Workload` row references an `Instance` row; the Instance carries
the Pterodactyl-shape fields (which are still useful for any
workload with resource limits, an owner, a node placement, etc.).

For workload kinds where a Pterodactyl-specific field doesn't
apply, the field is nullable in the schema and the workload
integration ignores it. For example, a `Website` workload's
`Instance.egg_id` is null (websites don't have eggs); a
`Database` workload's `Instance.startup` is null (databases use
`DatabaseSpec.init_script_ref` and `DatabaseSpec.readiness_probe`
instead).

This approach:

- Preserves the Pterodactyl migration importer contract verbatim.
- Avoids a parallel data model.
- Lets workload integrations use whatever subset of Instance fields
  makes sense for their kind.
- Adds the Workload abstraction as the *integration surface* for
  new workload kinds.

## 8. Workload-aware tabs

The expanded scope §4 requires that "a ZIM tab is a browser tab
that can display an ordinary website, a trusted internal page, a
managed workload, or an extension-provided interface." The
integration layer (`zim/integration/server_tab_feature/` from
`docs/architecture/BROWSER_INTEGRATION.md` §3) is renamed
conceptually to `WorkloadTabFeature` (the C++ class name; the
existing file paths are preserved for stability).

A workload tab's behavior is governed by the workload's `kind`:

- The tab's status indicator color reflects the workload's
  extended state, mapped through §6 above.
- The tab's context menu shows kind-specific items
  ("View database connections" for a Database; "View site logs"
  for a Website; "Rebuild" for a DevProcess with file-watch) via
  the integration's declared `context_menu_items`.
- The tab's default surface (`zim://workloads/{id}/console` for
  a GameServer; `zim://workloads/{id}/connections` for a Database;
  `zim://workloads/{id}/deploy` for a Website) is determined by
  the integration's declared `default_page`.

A tab whose workload is stopped, starting, unavailable, or hosted
on an unreachable remote node shows the corresponding state
gracefully (per the existing status model in
`docs/architecture/BROWSER_INTEGRATION.md` §3), without
misrepresenting the workload as "running" or hiding the
unavailability.

### URL inventory update

The `zim://` URL inventory in
`docs/architecture/BROWSER_INTEGRATION.md` §2 is updated to use
the workload terminology:

| URL                                          | What it loads                                       |
|----------------------------------------------|------------------------------------------------------|
| `zim://workloads`                            | Workload list (overview of all workloads).           |
| `zim://workloads/{id}`                       | Workload overview (kind-specific landing page).      |
| `zim://workloads/{id}/console`              | Console (only for ConsoleCapable workloads).         |
| `zim://workloads/{id}/files`                | Files (only for FilesystemCapable workloads).        |
| `zim://workloads/{id}/settings`             | Settings.                                            |
| `zim://workloads/{id}/backups`              | Backups (only for workloads with backup capability). |
| `zim://workloads/{id}/schedules`             | Schedules.                                           |
| `zim://workloads/{id}/metrics`               | Metrics.                                             |
| `zim://workloads/{id}/subusers`              | Subusers.                                            |
| `zim://workloads/{id}/network`               | Endpoints (only for NetworkCapable workloads).       |
| `zim://workloads/{id}/deploy`                | Deploy (only for Website workloads).                 |

Legacy `zim://servers/{id}/...` URLs continue to work (they map
to `zim://workloads/{id}/...` with the same suffix); this is
documented as a back-compat alias in
`docs/api/ZIM_API_SPEC.md` and is not a separate surface.

## 9. Permission vocabulary generalization

The `PermissionGrant` enum from
`docs/architecture/RUST_ENGINE.md` §3 is preserved (Pterodactyl
compatibility) and extended with workload-generalized aliases:

- `control.console` (existing) is interpreted as "send input to
  a `ConsoleCapable` runtime"; on a non-ConsoleCapable workload,
  requests with this grant return `Unsupported` (not
  `permission_denied` — the user has the permission, the workload
  just doesn't support the operation).
- New grants:
  - `workload.read` — read workload metadata.
  - `workload.create` — create a workload of any kind.
  - `workload.create.{kind}` — create a workload of a specific
    kind (e.g. `workload.create.database`).
  - `workload.delete` — delete a workload.
  - `workload.migrate` — initiate migration.
  - `workload.snapshot` — request a snapshot (only on
    `SnapshotCapable` workloads).
  - `workload.deploy` — deploy a new version (only on Website
    workloads).
  - `network.expose` — expose a new port (only on
    `NetworkCapable` workloads).
  - `network.unexpose` — unexpose a port.

The Pterodactyl-preserved grants (`control.start`, `control.stop`,
`control.restart`, `control.kill`) apply to the base lifecycle
and are valid for all workload kinds (every workload has a base
lifecycle, even if the semantics differ — for a Database, "stop"
means graceful shutdown waiting for in-flight transactions).

## 10. Avoidance of over-abstraction

The expanded scope §11 explicitly warns against premature
abstraction. The model above is deliberately bounded:

- Five logical layers, not five processes. The implementation
  may consolidate runtime adapters and workload integrations into
  fewer crates if the dependency graph turns out tighter than
  proposed (per `docs/architecture/RUST_ENGINE.md` §1's note about
  crate consolidation).
- Capability traits are introduced only where they represent a
  real semantic difference (a database genuinely has a readiness
  probe distinct from "process running"; a static website
  genuinely has no console). Capability traits are not introduced
  for cosmetic variation.
- The `Workload` type wraps `Instance` rather than replacing it,
  preserving the Pterodactyl migration contract without forcing a
  parallel data model.
- First-party workload integrations live in the engine workspace
  (`zim/engine/crates/workloads/`); they are not separate services.
- Third-party workload integrations are loaded into the engine
  process via the integration package manifest; they are not a
  separate process or a separate extension host. They are subject
  to compile-time Rust memory safety and runtime capability-grant
  enforcement.

The architecture is therefore *general* without being
*over-abstracted*: each abstraction (Workload kind, capability
trait, integration package) is justified by a concrete semantic
difference in the expanded scope, not by aesthetic preference.

## 11. What this abstraction does NOT do

- It does not force every workload into a Minecraft-shaped model.
  A database's spec does not include `startup: CommandTemplate` or
  `image: DockerImage`; it has `DatabaseSpec` with
  database-appropriate fields.
- It does not assume every workload has a console. A website's
  integration does not declare `ConsoleCapable`; calls to
  `attach_console` on a website workload return `Unsupported`.
- It does not assume every workload can be stopped by sending a
  generic signal. Each runtime adapter implements `stop()` with
  kind-appropriate semantics (a database adapter waits for
  in-flight transactions; a dev-process adapter sends SIGTERM and
  optionally kills the file-watcher).
- It does not assume every workload can be migrated by file copy.
  A database's `Migratable::migration_plan()` returns
  `requires_quiesce: true` and a multi-step plan including
  "checkpoint WAL", "stop accepting writes", "snapshot", "ship
  snapshot", "restore", "catch up WAL".
- It does not assume every workload uses the same networking.
  A game server has one allocation; a database has multiple
  endpoints; a static website has none (it's served by ZIM's own
  web server).
- It does not assume every workload has the same install/update
  procedure. The `Install` job (from
  `docs/architecture/RUST_ENGINE.md` §8) is general; the
  workload integration provides the install logic via
  `ExecCapable` (for game servers) or a kind-specific install
  trait (for databases: `DatabaseInstallable::init_db` and
  `DatabaseInstallable::apply_migrations`).
