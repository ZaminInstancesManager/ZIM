# Rust Engine Architecture (Phase 6)

This document specifies the Rust engine: its workspace organization,
domain model, supervision module, runtime adapters, persistence
layer, streaming, scheduling, jobs, node-runtime contract, and the
permitted dependency direction between crates.

It is canonical for the engine's internal structure. The API surface
the engine exposes is canonical in `docs/api/ZIM_API_SPEC.md`; this
document defines what's behind that surface.

## 1. Workspace organization

The Rust engine is a Cargo workspace under `zim/engine/`. The
proposed structure (final structure may consolidate based on real
dependency analysis during the first implementation milestone):

```
zim/engine/
├── Cargo.toml                   # workspace manifest
├── crates/
│   ├── domain/                  # zim_engine_domain: domain types, no I/O
│   ├── application/            # zim_engine_application: service operations, ports
│   ├── api/                    # zim_engine_api: HTTP + WS handlers
│   ├── persistence-sqlite/    # zim_engine_persistence_sqlite
│   ├── persistence-postgres/   # zim_engine_persistence_postgres (deferred)
│   ├── runtime-docker/         # zim_engine_runtime_docker
│   ├── runtime-host/           # zim_engine_runtime_host (TBD, opt-in)
│   ├── node-runtime/           # zim_engine_node_runtime (remote node contract)
│   ├── streaming/              # zim_engine_streaming: console + metrics fan-out
│   ├── scheduler/              # zim_engine_scheduler: per-instance cron
│   ├── jobs/                   # zim_engine_jobs: durable jobs (backup/transfer/install)
│   ├── backups/                # zim_engine_backups: backup domain
│   ├── transfers/              # zim_engine_transfers: transfer domain
│   ├── files/                  # zim_engine_files: file API
│   ├── eggs/                   # zim_engine_eggs: egg importer + zim.toml definition
│   ├── extensions/             # zim_engine_extensions: addon manifest + capability store
│   ├── auth/                   # zim_engine_auth: local users + OAuth
│   ├── authz/                  # zim_engine_authz: Permission vocabulary + capability enforcement
│   ├── audit/                  # zim_engine_audit: append-only audit log
│   ├── metrics/                # zim_engine_metrics: resource + operational metrics
│   ├── telemetry/             # zim_engine_telemetry (TBD, opt-in)
│   ├── platform/               # zim_engine_platform: OS-specific integration (launcher, service)
│   ├── frontend-loader/        # zim_engine_frontend_loader: default-frontend restoration
│   ├── migrate-pterodactyl/   # zim_engine_migrate_pterodactyl: Panel DB importer
│   └── main/                   # zim_engine_main: binary entrypoint
└── tests/                      # workspace integration tests
```

### Why a workspace, not a single crate

The prompt says: *"Do not create a separate crate for every noun.
Determine the actual dependency graph and split crates only when the
boundary provides meaningful independence, security, testability or
reuse."* The proposed split is justified by:

- **`domain` independence.** Domain types have zero I/O dependencies.
  This is the prompt's mandatory rule. A separate crate enforces it
  at compile time: any `use crate_x::HttpThing` in `domain/` is a
  compile error.

- **`application` port surface.** Application services use domain
  types and define *ports* (Rust traits) for persistence and runtime.
  Concretions live in adapter crates. The split makes the port
  boundary explicit and lets us swap adapters (e.g. SQLite → Postgres)
  without touching application code.

- **`runtime-docker` isolation.** The Docker runtime depends on
  `bollard` (or equivalent Docker client) and pulls in heavy
  dependencies; isolating it keeps the rest of the engine light and
  lets `runtime-host` (which has no Docker dependency) build on its
  own.

- **`node-runtime` separation.** The remote-node contract has its
  own versioning and its own threat model; isolating it makes the
  boundary explicit.

- **`migrate-pterodactyl` isolation.** The importer has dependencies
  on the Panel's database schema and the legacy egg format; it is
  build-on-its-own and can be omitted from a build that doesn't need
  migration support.

The other crates (streaming, scheduler, jobs, backups, transfers,
files, eggs, extensions, auth, authz, audit, metrics, telemetry,
platform, frontend-loader) are candidates for consolidation during
the first implementation milestone if their dependency graphs turn
out to be tightly coupled. The proposed split is a *starting point*;
the actual split will be informed by writing the code.

## 2. Permitted dependency direction

```
                    ┌────────────────────┐
                    │  zim_engine_main   │  (binary)
                    └─────────┬──────────┘
                              │
                              ▼
       ┌──────────────────────────────────────────┐
       │           zim_engine_api                  │  (HTTP+WS handlers)
       └─────────────────────┬────────────────────┘
                             │
                             ▼
       ┌──────────────────────────────────────────┐
       │       zim_engine_application               │  (service operations)
       └─────────┬────────────────────┬───────────┘
                 │                    │
                 ▼                    ▼
   ┌──────────────────────┐  ┌──────────────────────────────┐
   │ zim_engine_domain     │  │ zim_engine_{streaming,       │
   │ (no I/O, no deps)     │  │   scheduler, jobs, backups,   │
   └──────────────────────┘  │   transfers, files, eggs, ...}│
                              └──────────┬───────────────────┘
                                         │
                                         ▼
   ┌─────────────────────────────────────────────────────────┐
   │  zim_engine_{persistence_sqlite, runtime_docker,        │
   │    runtime_host, node_runtime, auth, authz, audit,        │
   │    metrics, telemetry, platform, frontend_loader,        │
   │    migrate_pterodactyl, extensions}                     │
   └─────────────────────────────────────────────────────────┘
```

Rules:

- `domain` ← depends on nothing inside the workspace, only on
  external crates that provide no I/O (e.g. `serde`, `chrono`,
  `uuid`).
- `application` ← depends on `domain` and on its own *port traits*.
  Does NOT depend on `api` or any concrete adapter.
- `api` ← depends on `application` (calls its services).
- Adapters ← depend on `application`'s port traits (to implement
  them) and on `domain` (to use domain types).
- `main` ← depends on `api`, `application`, and the chosen concrete
  adapters; wires them together.

If a developer adds `use zim_engine_api::Something` in `domain/`, it
must be a compile error. If a developer adds `use
zim_engine_persistence_sqlite::Something` in `application/`, it must
be a compile error. The crate graph enforces the boundary.

## 3. Domain model

The domain crate's core types (the prompt requires "the data model
for instances, instance configurations, nodes, allocations, server
definitions, users and permissions, runtime identities, jobs and
schedules, backups and transfers, extension registrations and grants,
events or event checkpoints where persistence is required, audit
records, custom icons and relevant metadata"):

```rust
// In zim_engine_domain

pub struct InstanceId(pub Uuid);
pub struct NodeId(pub Uuid);
pub struct UserId(pub Uuid);
pub struct AllocationId(pub u64);
pub struct EggId(pub Uuid);
pub struct JobId(pub Uuid);
pub struct BackupId(pub Uuid);
pub struct TransferId(pub Uuid);
pub struct ScheduleId(pub Uuid);
pub struct ExtensionId(pub Uuid);

pub struct Instance {
    pub id: InstanceId,
    pub external_id: Option<String>,         // legacy compat
    pub uuid: String,                        // legacy compat
    pub uuid_short: String,                 // legacy compat
    pub node_id: NodeId,
    pub name: String,
    pub description: String,
    pub status: InstanceStatus,             // enum, not string
    pub skip_egg_scripts: bool,
    pub owner_id: UserId,
    pub limits: ResourceLimits,
    pub allocation_id: AllocationId,
    pub egg_id: EggId,
    pub startup: CommandTemplate,            // typed, with variable substitution
    pub image: DockerImage,                  // typed, validated
    pub allocation_limit: Option<u32>,
    pub database_limit: Option<u32>,
    pub backup_limit: u32,
    pub crash_detection_enabled: bool,
    pub installed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub enum InstanceStatus {
    Offline,        // observed: not running; desired: stopped or crashed
    Starting,
    Running,
    Stopping,
    Installing,
    Suspended,
    RestoringBackup,
    Transferring,
    CrashLoop,      // ZIM extension: crashes exceeded threshold
    Unknown,        // ZIM extension: runtime unreachable / node disconnected
}

pub struct ResourceLimits {
    pub memory: Bytes,
    pub swap: Bytes,
    pub disk: Bytes,
    pub io_weight: u16,           // 1-1000, matches Wings
    pub cpu_millicores: u32,      // 1000 = 1 core
    pub threads: Option<String>,  // CPU pinning spec
    pub oom_disabled: bool,
}

pub struct Node {
    pub id: NodeId,
    pub name: String,
    pub location_id: LocationId,
    pub fqdn: String,             // for remote nodes
    pub scheme: Scheme,            // http / https
    pub public_port: u16,
    pub listen_port: u16,          // for remote node contract
    pub capabilities: NodeCapabilities,
    pub deployment: NodeDeployment, // Local / RemoteLinux / (TBD)
    pub last_heartbeat: Option<DateTime<Utc>>,
    pub last_seen_state: NodeState,
}

pub enum NodeDeployment {
    LocalLinux,       // engine and runtime on same machine
    RemoteLinux,      // remote node via authenticated contract
    // LocalWindows, LocalMacOS are *host* runtimes (ZIM extension),
    // not container runtimes; documented as constrained in
    // PLATFORM_SUPPORT.md.
}

pub struct Allocation {
    pub id: AllocationId,
    pub node_id: NodeId,
    pub ip: IpAddr,
    pub port: u16,
    pub assigned_to: Option<InstanceId>,
    pub note: Option<String>,
}

pub struct User {
    pub id: UserId,
    pub email: String,
    pub username: String,
    pub external_id: Option<String>,
    pub password_hash: Option<String>,        // local accounts
    pub oauth_subject: Option<String>,         // OAuth accounts
    pub is_admin: bool,
    pub root_admin: bool,
}

pub struct Permission {
    pub user_id: UserId,
    pub instance_id: InstanceId,
    pub grants: PermissionSet,                  // bitfield of canonical grants
}

pub struct PermissionSet(pub u64);  // bitfield of PermissionGrant variants

#[repr(u64)]
pub enum PermissionGrant {
    WebsocketConnect = 1 << 0,                  // matches Panel 'websocket.connect'
    ControlConsole = 1 << 1,
    ControlStart = 1 << 2,
    ControlStop = 1 << 3,
    ControlRestart = 1 << 4,
    FileRead = 1 << 5,
    FileReadContent = 1 << 6,
    FileWrite = 1 << 7,
    FileCreate = 1 << 8,
    FileDelete = 1 << 9,
    FileArchive = 1 << 10,
    BackupRead = 1 << 11,
    BackupCreate = 1 << 12,
    BackupUpdate = 1 << 13,
    BackupDelete = 1 << 14,
    BackupRestore = 1 << 15,
    BackupDownload = 1 << 16,
    AllocationRead = 1 << 17,
    AllocationCreate = 1 << 18,
    AllocationUpdate = 1 << 19,
    AllocationDelete = 1 << 20,
    UserRead = 1 << 21,
    UserCreate = 1 << 22,
    UserUpdate = 1 << 23,
    UserDelete = 1 << 24,
    ScheduleRead = 1 << 25,
    ScheduleCreate = 1 << 26,
    ScheduleUpdate = 1 << 27,
    ScheduleDelete = 1 << 28,
    DatabaseRead = 1 << 29,
    DatabaseCreate = 1 << 30,
    DatabaseUpdate = 1 << 31,
    DatabaseDelete = 1 << 32,
    DatabaseViewPassword = 1 << 33,
    SettingsRead = 1 << 34,
    SettingsUpdate = 1 << 35,
    Restart = 1 << 36,                          // 'control.restart' in some eggs; preserved
    Install = 1 << 37,                          // ZIM: install/reinstall
    Transfer = 1 << 38,                         // ZIM: transfer ownership
    AdminInstance = 1 << 39,                    // ZIM: instance admin
}

pub struct Job {
    pub id: JobId,
    pub kind: JobKind,                          // Backup / Transfer / Install / Update
    pub instance_id: InstanceId,
    pub state: JobState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub cleanup_pending: bool,
    pub generation: u64,                        // monotonic; for stale-event protection
}

pub enum JobState {
    Pending,
    InProgress { stage: JobStage, progress: f32 },
    Succeeded,
    Failed { reason: String },
    Cancelled,
    CleanupPending,
}

pub struct Backup {
    pub id: BackupId,
    pub instance_id: InstanceId,
    pub job_id: JobId,                          // link to the durable job
    pub uuid: String,
    pub file_name: String,
    pub bytes: u64,
    pub sha256_hash: Option<String>,
    pub locked: bool,
    pub completed_at: Option<DateTime<Utc>>,
}

pub struct Transfer {
    pub id: TransferId,
    pub instance_id: InstanceId,
    pub job_id: JobId,
    pub from_node_id: NodeId,
    pub to_node_id: NodeId,
    pub state: TransferState,
    pub archive_path: Option<String>,
}

pub enum TransferState {
    Pending,
    SourceStopping,
    SourceArchiving,
    ArchiveTransferring,
    DestExtracting,
    DestStarting,
    SourceCleanup,
    Succeeded,
    Failed,
    Cancelled,
}

pub struct Schedule {
    pub id: ScheduleId,
    pub instance_id: InstanceId,
    pub name: String,
    pub cron: String,                          // validated cron expression
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub is_active: bool,
    pub only_when_online: bool,
    pub actions: Vec<ScheduleAction>,          // power / command
}

pub struct AuditRecord {
    pub id: u64,
    pub timestamp: DateTime<Utc>,
    pub actor: AuditActor,                     // user / extension / system
    pub actor_id: Option<String>,
    pub action: String,
    pub instance_id: Option<InstanceId>,
    pub metadata: serde_json::Value,
    pub ip_address: Option<IpAddr>,
    pub user_agent: Option<String>,
}

pub struct ExtensionRegistration {
    pub id: ExtensionId,
    pub manifest: ExtensionManifest,
    pub version: String,
    pub installed_at: DateTime<Utc>,
    pub enabled: bool,
    pub granted_permissions: PermissionSet,
    pub signature: Option<ExtensionSignature>, // required for system-level extensions
}
```

The schema above is the *domain* model. The persisted schema (in the
chosen DB) is similar but storage-shaped (foreign keys, indexes,
denormalized where needed for query performance). The
domain↔persistence mapping is implemented in the
`persistence-*` adapter crates, not in `domain`.

## 4. Supervision module

The supervision module is the engine's analogue of Wings's `server/`
package. It owns per-instance state, lifecycle, power-action
serialization, crash detection, and the runtime abstraction.

```rust
// In zim_engine_application::supervision

pub struct Instance {
    pub id: InstanceId,
    inner: Arc<InstanceInner>,                  // shared with subscribers
}

struct InstanceInner {
    desired: Mutex<DesiredState>,               // start / stop / restart
    observed: RwLock<ObservedState>,             // from runtime
    power_queue: PowerQueue,                     // single-worker queue (Wings invariant)
    runtime: Box<dyn Runtime>,
    subscribers: StreamingSet,                  // console + metrics subscribers
    crash_state: CrashState,
    cfg: InstanceConfig,
    generation: AtomicU64,                       // bumped on every transition; for stale-event protection
}

pub enum DesiredState { Started, Stopped, Restarted, Killed, Suspended }

pub enum ObservedState {
    Offline { exit_code: Option<i32>, oom_killed: bool },
    Starting,
    Running { started_at: DateTime<Utc>, pid: Option<u32> },
    Stopping { since: DateTime<Utc> },
    Unknown,                                     // runtime unreachable
}

pub trait Runtime: Send + Sync {
    fn kind(&self) -> RuntimeKind;               // Container / Host
    fn state(&self) -> Result<ObservedState, RuntimeError>;
    fn start(&self, cfg: &InstanceConfig) -> Result<(), RuntimeError>;
    fn stop(&self, deadline: Duration) -> Result<(), RuntimeError>;
    fn kill(&self) -> Result<(), RuntimeError>;
    fn exec(&self, cmd: ExecCommand) -> Result<ExecOutput, RuntimeError>;
    fn attach_console(&self) -> Result<ConsoleStream, RuntimeError>;
    fn send_input(&self, input: &[u8]) -> Result<(), RuntimeError>;
    fn exit_state(&self) -> Result<(Option<i32>, bool), RuntimeError>;  // (exit_code, oom_killed)
    fn stats(&self) -> Result<ResourceUsage, RuntimeError>;
    fn generation(&self) -> u64;                // for stale-event protection
}

pub struct CrashState {
    last_crash: Mutex<Option<DateTime<Utc>>>,
    recent_crashes: Mutex<VecDeque<DateTime<Utc>>>,  // for crash-loop detection
    in_crash_loop: AtomicBool,
}
```

### Lifecycle correctness

The supervision module enforces the lifecycle invariants from
`ZIM_ARCHITECTURE.md` §4:

- Per-instance `Mutex<DesiredState>` ensures only one desired state
  is set at a time.
- `power_queue` is a single-worker queue (matches Wings's
  `workerpool` of size 1); concurrent start/stop/restart/kill are
  sequenced.
- `generation: AtomicU64` is bumped on every transition; runtime
  events whose generation is older than the current generation are
  ignored (stale-event protection).
- `CrashState` tracks the recent crash history; an instance that
  crashes N times within window T is placed in `crash_loop` state and
  is not auto-restarted.
- On engine restart, the engine walks every instance, calls
  `runtime.state()`, and reconciles `desired` ↔ `observed`; if
  `desired == Started` and `observed == Offline`, the engine queues
  a restart (subject to crash-loop protection).

### Runtime adapters

- `DockerRuntime` (`runtime-docker` crate) — uses `bollard` (Rust
  Docker client) to manage containers. Preserves Wings's `pterodactyl0`
  network setup behavior.
- `HostRuntime` (`runtime-host` crate, TBD, opt-in) — runs the
  process directly via `tokio::process::Command`, no isolation.
  Requires explicit admin consent per instance (per
  `docs/security/THREAT_MODEL.md`).
- `RemoteRuntime` (in `node-runtime` crate) — talks to a remote node
  process via the versioned, authenticated node-runtime contract;
  the remote node runs its own `DockerRuntime` (or `HostRuntime`).
  The remote runtime reports observed state to the engine; the
  engine never directly manages the remote node's containers.

## 5. Persistence and data ownership

The persistence adapter crate implements the persistence port
(`application::ports::Persistence`). The initial implementation is
`persistence-sqlite` for single-machine deployments. A
`persistence-postgres` adapter is deferred for multi-node deployments
where a single authoritative database is preferable to per-node
SQLite.

### Schema

The schema mirrors the domain model from §3, with:

- Foreign keys between `instances`, `nodes`, `allocations`,
  `users`, `permissions`, `eggs`, `schedules`, `jobs`, `backups`,
  `transfers`, `audit`, `extension_registrations`.
- Indexes on hot query paths: `instances(node_id)`,
  `allocations(node_id, assigned_to)`, `permissions(user_id,
  instance_id)`, `audit(timestamp)`, `jobs(state, instance_id)`.
- Separation of desired vs. observed state: the `instances` table
  stores desired configuration; a separate `instance_runtime_state`
  table (or a JSON column on `instances` for transient state) stores
  observed runtime state. The engine never trusts the persisted
  observed state as proof of liveness (per lifecycle invariant 1).

### Migrations

The engine's migrations are SQL files under
`zim/engine/crates/persistence-sqlite/migrations/`, named
`NNNN_description.sql` and applied in order. Migrations are
forward-only (no down-migrations; the engine does not support
schema rollbacks because ZIM's own metadata is small and a
rollback-via-restore is safer than a schema rollback).

A separate migration importer (`migrate-pterodactyl` crate) reads an
existing Pterodactyl Panel MySQL database and produces a ZIM SQLite
(or Postgres) database; see
`docs/compatibility/MIGRATION_AND_COMPATIBILITY.md` for details.

### Choice of database

The prompt says: *"Choose the initial database based on deployment
requirements and the actual consistency model. SQLite may suit a
single-machine installation; multi-node coordination may require a
different authoritative storage arrangement. Explain the decision
rather than committing to a technology by habit."*

- **Single-machine ZIM (default).** SQLite. The engine is the only
  writer; concurrent reads from the browser are fine. SQLite's
  durability guarantees (WAL mode, fsync on commit) are sufficient
  for the engine's per-instance metadata.
- **Multi-node ZIM (with remote Linux nodes).** Each remote node has
  its own local SQLite for the runtime state it owns; the central
  engine has its own SQLite (or Postgres if the deployment grows)
  for the authoritative instance/node/allocation/user state. The
  engine never treats a remote node's local DB as authoritative for
  anything except that node's observed state.

The architecture supports both because the persistence is a port;
swapping SQLite for Postgres is a matter of writing the
`persistence-postgres` adapter.

## 6. Streaming (console + metrics)

The streaming module (`zim_engine_streaming`) owns the fan-out from
runtime → engine → subscribers.

```rust
pub trait StreamSource: Send + Sync {
    fn subscribe(&self, filter: &StreamFilter) -> StreamHandle;
}

pub struct StreamHandle {
    pub id: SubscriberId,
    pub rx: mpsc::Receiver<StreamEvent>,
}

pub enum StreamEvent {
    ConsoleOutput { generation: u64, ts: DateTime<Utc>, bytes: Bytes },
    StateChange { generation: u64, from: ObservedState, to: ObservedState },
    ResourceUsage { generation: u64, ts: DateTime<Utc>, usage: ResourceUsage },
    InstallProgress { generation: u64, stage: JobStage, progress: f32 },
    JobProgress { generation: u64, job_id: JobId, stage: JobStage, progress: f32 },
    End,
}

pub struct StreamFilter {
    pub instance_id: InstanceId,
    pub kinds: EnumSet<StreamEventKind>,
    pub since_generation: Option<u64>,           // for replay-from-checkpoint
}
```

### Replay-from-checkpoint

Each WS subscriber includes its last-seen generation in the
`Sec-WebSocket-Protocol` header on connect (or as a query parameter
on the HTTP upgrade). The engine replays any events with generation
greater than the subscriber's last-seen, then live-streams. This
handles reconnects after a transient WS drop.

### Backpressure

If a subscriber's channel is full, the engine drops the *oldest* event
in the channel (not the newest) and increments a `dropped_events`
counter on the subscriber; the subscriber's UI surfaces a "X events
dropped" notice. This is the explicit backpressure strategy — older
console output is less valuable than newer, and dropping old is
better than blocking the runtime.

## 7. Scheduling

The scheduler module (`zim_engine_scheduler`) reproduces Pterodactyl's
per-instance cron. It uses `tokio-cron-scheduler` (or equivalent Rust
cron library) to evaluate schedule expressions and dispatch schedule
actions.

Each `Schedule` belongs to an `Instance`. When a schedule fires:

1. If `only_when_online == true` and the instance is not `Running`,
   the schedule is skipped (recorded in audit log).
2. Otherwise, the schedule's actions are queued on the instance's
   power queue (or console input queue) — same serialization as
   user-initiated actions.

Schedules survive engine restart (persisted). On restart, the
scheduler re-evaluates `next_run_at` for each schedule.

## 8. Durable jobs

The jobs module (`zim_engine_jobs`) owns long-running operations:
backup, transfer, install, update. Each is a durable job with an
identity (`JobId`), a state machine, and resumable semantics.

- **Backup job:** `Pending → InProgress(stage=Archive, progress)
  → Succeeded` or `Failed`. The archive is written to the
  configured backup storage; the `Backups` row's `sha256_hash` and
  `bytes` are recorded on success.
- **Transfer job:** `Pending → SourceStopping → SourceArchiving
  → ArchiveTransferring → DestExtracting → DestStarting →
  SourceCleanup → Succeeded`. Two-phase commit semantics; a partial
  failure leaves the job in `Failed` state with `cleanup_pending =
  true`; a background cleaner completes the cleanup.
- **Install job:** `Pending → InProgress(stage=PullingImage,
  progress) → InProgress(stage=RunningInstallScript, progress) →
  Succeeded` or `Failed`. The install script runs in a constrained
  container; untrusted egg install scripts never run with engine
  privileges.
- **Update job:** like install, but for egg updates.

All jobs survive engine restart. On restart, the engine reconciles
jobs: `InProgress` jobs are re-evaluated; the runtime's actual state
is checked (e.g. if a backup job was `InProgress` when the engine
crashed, the partial archive file is deleted and the job is marked
`Failed`).

## 9. Node-runtime contract

The node-runtime contract (`zim_engine_node_runtime`) is the
authenticated, versioned contract between the engine and a remote
node. It is **not** the Panel↔Wings contract (which was
unidirectional: Panel pushed to Wings). The ZIM contract is
bidirectional:

- The engine sends *desired state* to the remote node ("start
  instance X", "stop instance Y").
- The remote node sends *observed state* and *events* to the engine
  (state changes, console output, resource usage).

The wire format is **HTTPS+WSS** (not gRPC), to keep the contract
consistent with the engine's local API surface and to avoid a
separate binary protocol.

### Authentication

The remote node authenticates to the engine with a node-scoped
capability token (issued by the engine when the node is registered,
rotated periodically). The engine authenticates to the remote node
with a node-specific shared secret stored in the engine's secrets
store.

### Failure detection

Heartbeat: the remote node sends a heartbeat every N seconds. If
the engine does not receive a heartbeat within `2*N + grace`, the
node is marked `disconnected` and its instances are marked
`Unknown` (not `Stopped`).

### Versioning

The contract has an explicit version number; the engine and the
remote node negotiate the highest version both support. A version
mismatch results in the node being marked `version_mismatch` and
its instances being marked `Unknown` until both sides are upgraded.

## 10. Frontend loader

The frontend-loader module (`zim_engine_frontend_loader`) serves the
default and custom frontend packages to the browser.

- The engine stores the active frontend package's bytes and manifest
  in its data directory.
- When the browser requests `zim://newtab` (or any `zim://` page
  that is part of the frontend), the engine's WebUI handler reads
  the active frontend package and serves the page.
- Before activating a new frontend package, the engine runs a
  *smoke test*: it loads the manifest, checks it against the schema,
  checks the declared API version against the engine's API version,
  and executes a configured smoke-test page (a tiny HTML page that
  exercises the basic API). If the smoke test fails, the engine
  refuses to activate the new package and keeps the previous one
  (or falls back to the default frontend).
- The default frontend is bundled with the engine and cannot be
  removed; only overridden.

## 11. What this engine is NOT

- It is not a thin wrapper around Wings's Go source. The Rust engine
  is a clean reimplementation guided by Wings's behavioral contracts,
  not a port of its source code.
- It is not a multi-backend system. There is one engine; the
  Panel↔Wings split is gone.
- It is not a generic process supervisor. It is a server-management
  engine; generic process supervision is a means, not the product.
- It is not a Chromium rendering-engine replacement. The Rust engine
  does not touch Chromium's rendering pipeline.
