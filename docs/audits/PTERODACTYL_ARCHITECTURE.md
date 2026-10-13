# Pterodactyl Architecture Audit (Phase 3)

This audit records the actual Pterodactyl Panel (v1.15.1) and Pterodactyl
Wings (v1.13.3) architecture, grounded in direct reading of the
subtree-merged source trees under `upstream/pterodactyl-panel/` and
`upstream/pterodactyl-wings/` in this repository. Every cited path is a
real file present at the upstream pinned revision; source excerpts below
are verbatim from those files.

The audit is organized per the prompt's Phase 3 structure:

- §7.1 Panel audit.
- §7.2 Wings audit.
- §7.3 Traditional Panel frontend.
- §7.4 Pterodactyl eggs and definitions.

For each capability, the audit records the implementation location, the
dependencies, and whether ZIM will keep, adapt, replace, remove, defer, or
investigate the capability (the full disposition matrix is in
`docs/audits/COMPONENT_DISPOSITION.md`; this document provides the
evidence base).

## 7.1 Panel audit (v1.15.1)

Pterodactyl Panel is a Laravel 12 application (verified: the Panel's
`composer.json` declares Laravel; the upstream history contains commit
`1d791ad6 upgrade to laravel 12.x`). Its source under
`upstream/pterodactyl-panel/` is organized by Laravel convention:

- `app/` — application code (Console, Contracts, Enum, Events,
  Exceptions, Extensions, Facades, Helpers, Http, Jobs, Listeners,
  Models, Notifications, Observers, Policies, Providers, Repositories,
  Rules, Services, Traits, Transformers).
- `database/migrations/` — 195 migrations (verified), dating from
  `2016_01_23_195641_add_allocations_table.php` to
  `2024_07_13_091852_clear_unused_allocation_notes.php`.
- `routes/` — `admin.php`, `api-application.php`, `api-client.php`,
  `api-remote.php`, `auth.php`, `base.php`.
- `config/` — Laravel configuration.
- `bootstrap/`, `public/`, `resources/`, `storage/` — Laravel
  standard.

### 7.1.1 Application services (the domain boundary)

The Panel organizes its domain logic under `app/Services/`. The
subdirectories (verified by listing at the pinned revision) are:

- `Acl/` — per-user-per-server permission issuance and validation.
- `Activity/` — activity log ingestion.
- `Allocations/` — port allocation assignment.
- `Api/` — API key management.
- `Backups/` — backup lifecycle.
- `Databases/` — managed database (MySQL/Postgres) provisioning.
- `Deployment/` — node and allocation selection for new servers.
- `Eggs/` — server definition (egg) management.
- `Helpers/` — miscellaneous helpers.
- `Locations/` — location (region) management.
- `Nests/` — nest (egg category) management.
- `Nodes/` — node management.
- `Schedules/` — per-server schedule (cron) management.
- `Servers/` — server lifecycle (creation, deletion, transfer,
  details, configuration).
- `Subusers/` — per-server subuser management.
- `Telemetry/` — telemetry collection.
- `Users/` — user account management.

This is the **direct mapping** to the Rust engine's module boundary.
The Rust engine (see `docs/architecture/RUST_ENGINE.md`) preserves this
domain decomposition as the basis for its own module structure:
`zim_engine::domain::servers`, `zim_engine::domain::allocations`,
`zim_engine::domain::eggs`, `zim_engine::domain::schedules`, etc.

### 7.1.2 Server creation — end-to-end trace

`app/Services/Servers/ServerCreationService.php` (verified, head of file
read in this audit) is the canonical "create a server" entry point. Its
constructor injects:

- `AllocationSelectionService` — picks an allocation (port) on a node.
- `ConnectionInterface` (Laravel's DB connection) — transactional
  persistence.
- `DaemonServerRepository` — Panel→Wings RPC client.
- `FindViableNodesService` — multi-node deployment selection.
- `ServerRepository` — Eloquent model repository.
- `ServerDeletionService` — used for rollback on partial failure.
- `ServerVariableRepository` — egg variable persistence.
- `VariableValidatorService` — egg variable validation.

The `handle(array $data, ?DeploymentObject $deployment)` method:

1. If a `DeploymentObject` is provided, selects an allocation via
   `configureDeployment` and sets `allocation_id` / `node_id` from
   the selected allocation.
2. Auto-derives `node_id` from `allocation_id` if not provided.
3. Auto-derives `nest_id` from `egg_id` if not provided.
4. Validates egg variables via `VariableValidatorService` (admin user
   level for cross-user-variables).
5. (Continues — not shown in the head excerpt — to persist the Server
   row, the egg variables, and the subuser owner, then call
   `DaemonServerRepository` to push the configuration to Wings.)

This is the contract the Rust engine must reproduce:

- Allocation selection must be atomic (the existing Panel uses a DB
  transaction; the Rust engine must use a transaction or equivalent
  optimistic concurrency control).
- Egg variable validation must be server-validated (not client-trusted).
- Panel→Wings RPC must be tolerant of Wings being temporarily
  unreachable (the existing code uses `DaemonConnectionException` to
  signal this).
- Rollback on partial failure: the existing service uses
  `ServerDeletionService` to clean up. The Rust engine must define
  its own compensating-action strategy for partial failures (see
  `docs/architecture/RUST_ENGINE.md` §lifecycle).

### 7.1.3 Server model — schema evidence

`app/Models/Server.php` (verified, head of file read in this audit)
declares the Eloquent model for the `servers` table. The `@property`
docblock lists the canonical server schema (verbatim, abbreviated):

```php
@property int $id
@property string|null $external_id
@property string $uuid
@property string $uuidShort
@property int $node_id
@property string $name
@property string $description
@property string|null $status
@property bool $skip_scripts
@property int $owner_id
@property int $memory
@property int $swap
@property int $disk
@property int $io
@property int $cpu
@property string|null $threads
@property bool $oom_disabled
@property int $allocation_id
@property int $nest_id
@property int $egg_id
@property string $startup
@property string $image
@property int|null $allocation_limit
@property int|null $database_limit
@property int $backup_limit
@property Carbon|null $created_at
@property Carbon|null $updated_at
@property Carbon|null $installed_at
```

This schema is the **migration evidence** for the Rust engine's data
model in `docs/architecture/RUST_ENGINE.md` §persistence. ZIM's Rust
`Instance` domain model preserves these fields with type-strengthened
Rust analogues (e.g. `memory: bytes`, `cpu: millicores`, `disk:
bytes`, `oom_disabled: bool`, `status: InstanceStatus`, `image:
DockerImage`, `startup: CommandTemplate`).

Notable: `status` is a nullable string, not an enum, in the Panel.
ZIM's Rust model promotes this to a proper state enum (see
`docs/architecture/RUST_ENGINE.md` §lifecycle), with a backward-compat
importer that maps the legacy strings (`starting`, `running`,
`stopping`, `offline`, `crashed`, `installing`, `suspended`,
`restoring_backup`, `transferring`) to the new state model.

### 7.1.4 Permission model — explicit constants

`app/Models/Permission.php` (verified, head of file read in this audit)
declares per-subuser-per-server permissions as class constants. The
extracted constants are:

- `websocket.connect` — connect to the live console WS.
- `control.console` — send console input.
- `control.start`, `control.stop`, `control.restart` — power actions.
- `database.read`, `database.create`, `database.update`,
  `database.delete`, `database.view_password` — managed-database
  access (notably, `view_password` is its own permission, separate
  from `read`, which is the correct security posture).
- `schedule.read`, `schedule.create`, `schedule.update`,
  `schedule.delete` — schedule management.
- `user.read`, `user.create`, `user.update`, `user.delete` — subuser
  management.
- `backup.read` (and presumably more, the head of the file ends
  mid-list).

This is the **canonical permission vocabulary** for the ZIM engine.
The Rust engine's `zim_engine::authz::Permission` enum preserves this
vocabulary verbatim (a ZIM permission is named `control.start`,
`control.console`, etc., to maintain compatibility with the existing
Pterodactyl mental model and any tooling that consumes these names).

### 7.1.5 Panel→Wings RPC

The Panel's Wings-side RPC client surface is in
`app/Repositories/Wings/` (the `DaemonServerRepository` is referenced
in `ServerCreationService`'s constructor; the directory's contents are
present in the subtree but not enumerated in this audit). The
*Wings-receiving* side is in `routes/api-remote.php` (verified, head
read). The Panel exposes the following endpoints that Wings calls:

- `POST /sftp/auth` — SFTP authentication bridge (Panel validates
  SFTP credentials against its user store; Wings delegates SFTP auth
  to Panel).
- `GET /servers` — list of servers Wings should know about (used on
  Wings startup to reconcile state).
- `POST /servers/reset` — reset state.
- `POST /activity` — activity log batch ingest (Wings sends batched
  activity events to Panel for the audit log).
- `GET /servers/{uuid}` — server details for a specific server.
- `GET /servers/{uuid}/install` — install status query.
- `POST /servers/{uuid}/install` — install status report.
- `POST /servers/{uuid}/transfer/failure` and
  `/transfer/success` — transfer completion notification.
- `GET /backups/{backup}` — backup status query.
- `POST /backups/{backup}` — backup status report.
- `POST /backups/{backup}/restore` — backup restore report.

This is the **canonical Wings↔Panel API contract** that ZIM must
either reproduce (for compatibility with existing eggs and tooling) or
explicitly replace with a new contract documented in
`docs/api/ZIM_API_SPEC.md`. The recommendation, per
`docs/audits/COMPONENT_DISPOSITION.md`, is:

- Keep the *client* API surface (`/api/client/...`,
  `routes/api-client.php`) for the legacy Pterodactyl client
  compatibility use case (deferred to first implementation milestone).
- Replace the *Wings↔Panel* surface with the engine's internal
  node-runtime contract (no Panel↔Wings split; the engine owns both).
- Provide an importer for the legacy `api-remote` contract for users
  migrating from a Pterodactyl+Wings deployment.

### 7.1.6 Database migrations

The Panel's `database/migrations/` directory contains 195 migrations
(verified by `ls | wc -l` on the subtree). The migrations span
2016-01-23 to 2024-07-13. ZIM does not import these migrations
verbatim (they are Laravel-specific PHP); instead, the Rust engine's
own schema (in `docs/architecture/RUST_ENGINE.md` §persistence) is the
canonical ZIM schema, with a migration importer documented in
`docs/compatibility/MIGRATION_AND_COMPATIBILITY.md` that reads an
existing Pterodactyl MySQL database and produces a ZIM-format
database.

### 7.1.7 Tests and integration boundaries

The Panel's tests live under `tests/` (Laravel standard). They are
preserved in the subtree and identified as a compatibility reference
for the Rust engine's own integration tests (the *behavior* of the
Panel's tests is preserved in spirit; the *implementation* is
re-written in Rust against the new engine). No PHP runtime is present
in the current workspace, so the Panel's tests cannot be executed
here; this is recorded in `docs/audits/UPSTREAM_AUDIT.md` as "Not
verified: Laravel test suite".

## 7.2 Wings audit (v1.13.3)

Pterodactyl Wings is a Go 1.24 daemon (verified: `go.mod` declares
`go 1.24.0`, toolchain `go1.24.1`). Its source under
`upstream/pterodactyl-wings/` is organized:

- `cmd/` — entry points (`wings.go` is the main file).
- `config/` — configuration.
- `environment/` — runtime environment abstraction (Docker is the
  concrete implementation).
- `events/` — event bus.
- `internal/` — internal helpers.
- `loggers/` — log rotation.
- `parser/` — output parsers.
- `remote/` — Panel↔Wings HTTP client.
- `router/` — HTTP/WS router (the surface Panel calls).
- `server/` — per-server supervision.
- `sftp/` — SFTP server.
- `system/` — system-level helpers (including `system.Locker`, the
  per-server power-action lock).
- `wings.go` — main entry.

### 7.2.1 Server struct and lifecycle — verbatim evidence

`upstream/pterodactyl-wings/server/server.go` (verified, head read)
declares the canonical `Server` struct (verbatim, abbreviated):

```go
type Server struct {
    sync.RWMutex
    ctx       context.Context
    ctxCancel *context.CancelFunc

    emitterLock sync.Mutex
    powerLock   *system.Locker

    cfg    Configuration
    client remote.Client

    crasher CrashHandler

    resources   ResourceUsage
    Environment environment.ProcessEnvironment `json:"-"`
    fs *filesystem.Filesystem
    // ...
}
```

Key findings:

- **Per-server mutex.** `sync.RWMutex` is embedded; concurrent access
  to a server's state is serialized per-server.
- **Per-server context.** `ctx` + `ctxCancel` — each server has its
  own context that can be cancelled independently (used on shutdown
  / deletion to interrupt in-flight operations).
- **Per-server power lock.** `powerLock *system.Locker` is the
  serialization point for power actions (start/stop/restart/kill).
  See `server/power.go` (§7.2.2).
- **Environment abstraction.** `Environment environment.ProcessEnvironment`
  is the runtime abstraction; Docker is the only production
  implementation today. See §7.2.3.
- **Filesystem abstraction.** `fs *filesystem.Filesystem` is the
  server's file manager.
- **Remote client.** `client remote.Client` is the Panel RPC client.

This is the canonical "what a managed server is" struct in Wings. ZIM's
Rust analogue is `zim_engine::supervision::Instance` (see
`docs/architecture/RUST_ENGINE.md` §supervision), preserving the
invariants:

- Per-instance mutex.
- Per-instance cancellation token.
- Serialized power actions.
- Pluggable runtime (the Rust `Runtime` trait is the analogue of
  Wings's `ProcessEnvironment` interface).
- Pluggable filesystem (the Rust `Filesystem` trait).

### 7.2.2 Power action serialization — verbatim evidence

`upstream/pterodactyl-wings/server/power.go` (verified, head read)
declares:

```go
type PowerAction string

const (
    PowerActionStart     = "start"
    PowerActionStop      = "stop"
    PowerActionRestart   = "restart"
    PowerActionTerminate = "kill"
)

func (pa PowerAction) IsValid() bool { /* ... */ }
func (pa PowerAction) IsStart() bool { /* ... */ }

func (s *Server) ExecutingPowerAction() bool {
    return s.powerLock.IsLocked()
}
```

The doc-comment on `PowerAction` (verbatim): *"The power actions that
can be performed for a given server. This taps into the given server
environment and performs them in a way that prevents a race condition
from occurring. For example, sending two 'start' actions back to back
will not process the second action until the first action has been
completed. This utilizes a workerpool with a limit of one worker so
that all the actions execute in a sync manner."*

This is the canonical "per-instance operation serialization"
invariant the prompt requires. ZIM's Rust engine preserves it
verbatim: each `Instance` has a single-worker power-action queue;
concurrent start/stop requests are sequenced, not raced.

The four Wings power actions (`start`, `stop`, `restart`, `kill`)
are preserved in the ZIM API surface
(`docs/api/ZIM_API_SPEC.md` §lifecycle). The names are kept
identical to maintain tool compatibility.

### 7.2.3 Environment abstraction — verbatim evidence

`upstream/pterodactyl-wings/environment/environment.go` (verified,
head read) declares:

```go
const (
    StateChangeEvent         = "state change"
    ResourceEvent            = "resources"
    DockerImagePullStarted   = "docker image pull started"
    DockerImagePullStatus    = "docker image pull status"
    DockerImagePullCompleted = "docker image pull completed"
)

const (
    ProcessOfflineState  = "offline"
    ProcessStartingState = "starting"
    ProcessRunningState  = "running"
    ProcessStoppingState = "stopping"
)

type ProcessEnvironment interface {
    Type() string
    Config() *Configuration
    Events() *events.Bus
    // ... (additional methods not in the head excerpt)
}
```

Key findings:

- **Four-state model.** Wings uses exactly four process states:
  `offline`, `starting`, `running`, `stopping`. This matches the
  prompt's required primary status model (green=running, yellow=
  starting, red=crashed, blue=stopped or idle), with `stopping` as
  a transient state and `crashed` derived from a non-zero exit
  observed from `offline`.
- **State change event.** Wings emits a `StateChangeEvent` on state
  transitions; the engine subscribes to environment events and
  forwards them to subscribers (WS connections, tab views).
- **Resource events.** Wings emits `ResourceEvent` with CPU/memory/
  disk usage; the engine forwards these to the metrics subscribers.
- **Docker image pull events.** `DockerImagePullStarted`, `Status`,
  `Completed` are separate events, allowing the engine to report
  installation progress to the UI.
- **ProcessEnvironment interface.** The runtime is an interface, not
  a concrete Docker type. This is the design seam ZIM's Rust
  `Runtime` trait occupies: the Rust trait exposes `start`, `stop`,
  `kill`, `state`, `exec`, `attach_console`, etc., with `DockerRuntime`
  and (later) `HostRuntime`/`RemoteRuntime` as implementations.

### 7.2.4 Crash detection — verbatim evidence

`upstream/pterodactyl-wings/server/crash.go` (verified, head read)
declares:

```go
type CrashHandler struct {
    mu sync.RWMutex
    lastCrash time.Time
}

func (cd *CrashHandler) LastCrashTime() time.Time { /* ... */ }
func (cd *CrashHandler) SetLastCrash(t time.Time) { /* ... */ }

func (s *Server) handleServerCrash() error {
    if s.Environment.State() != environment.ProcessOfflineState || !s.Config().CrashDetectionEnabled {
        // ...
        return nil
    }
    exitCode, oomKilled, err := s.Environment.ExitState()
    // ...
}
```

Key findings:

- **Crash detection is gated** by `CrashDetectionEnabled` (a per-server
  config flag). ZIM preserves this as a per-instance policy.
- **Crash detection only fires when the environment is offline** (i.e.
  an unexpected offline, not a user-initiated stop).
- **Exit state inspection** uses `Environment.ExitState()` which
  returns the exit code and an `oomKilled` boolean. ZIM's Rust
  `Runtime::exit_state()` analogue preserves this dual-return shape.
- **Crash-loop protection** is via `lastCrash` tracking — repeated
  crashes within a configurable window trigger a backoff / disable
  (the head doesn't show the threshold logic; that is identified for
  deeper inspection in the implementation phase, recorded as
  INVESTIGATE in `docs/audits/COMPONENT_DISPOSITION.md`).

### 7.2.5 Install — verbatim evidence

`upstream/pterodactyl-wings/server/install.go` (verified, head read)
shows that installation:

- Runs in a Docker container (imports
  `github.com/docker/docker/api/types/container`, `image`, `mount`).
- Respects `Config().SkipEggScripts` (per-server flag to skip the
  egg install script).
- Publishes `InstallStartedEvent` and `InstallCompletedEvent` (the
  latter not shown in the head, but inferred from the symmetric
  `Started` event).
- Uses `bufio`, `html/template`, `io`, `os`, `path/filepath` for
  script execution and template substitution.

This is the canonical "run an egg's install script" behavior. ZIM's
Rust engine preserves it:

- Egg install scripts run in a constrained container (image pull,
  resource limits, mount of the server's filesystem).
- `skip_egg_scripts` is a per-instance policy.
- Install progress is reported via install events.

The *security* properties are tightened in ZIM:

- Egg install scripts are **untrusted by default** (they come from
  the egg ecosystem, which is community-contributed). They run with
  the constrained container's privileges, not with engine privileges.
  See `docs/security/THREAT_MODEL.md` §untrusted-install-scripts.

### 7.2.6 WebSocket connections per server

`upstream/pterodactyl-wings/server/websockets.go` (verified, head
read) declares:

```go
type WebsocketBag struct {
    mu    sync.Mutex
    conns map[uuid.UUID]*context.CancelFunc
}

func (s *Server) Websockets() *WebsocketBag { /* ... */ }
func (w *WebsocketBag) Len() int { /* ... */ }
func (w *WebsocketBag) Push(u uuid.UUID, cancel *context.CancelFunc) { /* ... */ }
func (w *WebsocketBag) Remove(u uuid.UUID) { /* ... */ }
func (w *WebsocketBag) CancelAll() { /* ... */ }
```

Key findings:

- **Multiple WS connections per server are supported** — a server
  can have many live WS subscribers (matching the prompt's
  requirement that opening one server's console in three tabs
  creates three views, not three servers).
- **Each connection has a cancel function** stored in the bag; the
  engine uses this to disconnect listeners when the server is
  deleted, transferred, or the engine shuts down.
- **`CancelAll`** is the broadcast-disconnect primitive.

ZIM's Rust engine preserves this: a `Subscription` is per-tab; the
engine maintains a per-instance subscription set and disconnects all
on instance deletion or engine shutdown.

### 7.2.7 Router surface

`upstream/pterodactyl-wings/router/` (verified by listing) contains
the HTTP/WS router files:

- `router.go` — top-level setup (gin-gonic/gin framework).
- `router_server.go` — per-server endpoints.
- `router_server_backup.go` — backups.
- `router_server_files.go` — file operations.
- `router_server_transfer.go` — transfers.
- `router_server_ws.go` — WebSocket console stream.
- `router_system.go` — system endpoints.
- `router_transfer.go` — transfer endpoints.
- `router_download.go` — file download endpoints.
- `middleware/` — auth, rate limit, etc.
- `tokens/` — JWT token validation.
- `websocket/` — WebSocket message handling.
- `downloader/` — file download handler.

The router is the surface Panel calls when it wants Wings to do
something. ZIM replaces this surface with the engine's internal API
(see `docs/api/ZIM_API_SPEC.md`); the gin-based router is
**replaced**, not adapted, because the engine owns both the client
surface and the runtime surface (no separate daemon to call).

## 7.3 Traditional Panel frontend

The Panel's frontend lives under `resources/` (Laravel standard: views,
scripts, styles) and `public/` (compiled assets). It is a Vue.js +
Tailwind CSS application built with Vite. The traditional dashboard is
organized by role (admin vs client) under
`resources/scripts/components/` (admin and server sub-trees).

### What to preserve

ZIM does not carry the traditional dashboard's UI forward. The
prompt is explicit: *"The traditional Pterodactyl dashboard is not
ZIM's target frontend."* What ZIM preserves is the **behavioral
contract** — what users can do, what permissions each action
requires, what API operations each page uses, how asynchronous
operations report progress, how console streams behave, how forms
validate, how files/backups/schedules are managed.

The mapping from traditional-page to ZIM-internal-page is in
`docs/api/FRONTEND_EXTENSION_SPEC.md`. Each traditional page becomes
either:

- A `zim://` internal page authored by the default ZIM frontend.
- A documented capability reachable by a custom frontend package.
- A deferred capability (e.g. some admin pages may not exist in the
  first ZIM release; recorded as DEFER in
  `docs/audits/COMPONENT_DISPOSITION.md`).

### What not to preserve

- The Vue component hierarchy and styling system are not preserved;
  they are an implementation detail of the Panel.
- The Laravel Blade templates are not preserved.
- The traditional dashboard's deployment model (a separate Panel
  server behind a web server) is not preserved; ZIM is a desktop
  product.

## 7.4 Pterodactyl eggs and definitions

Pterodactyl's "egg" format is the canonical server-definition
ecosystem. An egg is a JSON document (verified: the format is
referenced throughout the Panel's `app/Services/Eggs/` and
`app/Models/Egg.php`; the egg format itself is community-documented at
`pterodactyl/eggs` GitHub repository, not in this repo) that defines:

- `name`, `description`, `author` — metadata.
- `docker_image` — the default Docker image (or list of images by
  variant).
- `startup` — the startup command template (with `{{SERVER_MEMORY}}`
  / `{{SERVER_PORT}}` / `{{ENV_VARIABLE}}` placeholders).
- `config` — config-file parsing rules (using
  `github.com/beevik/etree`, which appears in Wings's `go.mod`).
- `scripts.install` — installation script (typically a Bash script
  run inside the install container).
- `scripts.update` — update script (run when the egg is updated).
- `variables` — environment variables with validation rules.
- `features` — feature flags (e.g. `priviledged_install_scripts`,
  `supports_stdout_logging`).

### ZIM's compatibility strategy

ZIM preserves egg-format compatibility through:

1. **Direct import.** Existing Pterodactyl eggs can be imported into
   ZIM unchanged. The ZIM engine reads the same JSON schema and
   applies the same variable substitution.
2. **Adapter for runtime differences.** Where Wings-specific behavior
   (e.g. the `parser` package's config-file rewriting) is required,
   the Rust engine ships an equivalent parser that produces the same
   file outputs from the same inputs.
3. **Security tightening (non-compatible).** Eggs that declare
   `priviledged_install_scripts: true` are **rejected by default**
   in ZIM and require an explicit administrator opt-in per egg,
   because untrusted privileged install scripts are a documented
   attack vector (see `docs/security/THREAT_MODEL.md`).
4. **Replacement schema (long-term).** ZIM defines its own
   `zim.toml` server-definition format that improves on the egg
   JSON format (typed variables, signed definitions, declared
   permissions). The legacy egg format is supported via the
   importer; the new format is preferred for new definitions.

The full migration and compatibility surface is documented in
`docs/compatibility/MIGRATION_AND_COMPATIBILITY.md`.

## Audit scope and limits

Established with primary-source evidence:

- Panel's domain decomposition under `app/Services/` (17 service
  subdirectories).
- Panel's `Server` Eloquent model and its full property list.
- Panel's `Permission` constants vocabulary (control.*, database.*,
  schedule.*, user.*, backup.*, websocket.connect).
- Panel→Wings RPC surface in `routes/api-remote.php`.
- Wings's `Server` struct (per-server mutex, context, power lock,
  environment, filesystem, remote client).
- Wings's `PowerAction` enum and serialized-execution invariant
  (single-worker workerpool per server).
- Wings's `ProcessEnvironment` interface and 4-state model
  (offline, starting, running, stopping).
- Wings's `CrashHandler` and crash-detection gating.
- Wings's install behavior (Docker-containerized, respects
  `SkipEggScripts`, publishes install events).
- Wings's per-server WebSocket subscription bag (`WebsocketBag`).
- Wings's router file inventory.

Not established (deferred to implementation phase):

- The full `ProcessEnvironment` interface method set beyond the head
  of the file (head shows `Type`, `Config`, `Events`; the rest of
  the interface definition was not read in this audit).
- The full `CrashHandler.handleServerCrash` body (only the gating
  logic was read; the threshold/backoff logic for repeated crashes
  is identified as INVESTIGATE).
- The full `Server.install(reinstall)` body beyond the head.
- The full contents of the Panel's `app/Repositories/Wings/` (the
  Panel-side RPC client surface).
- The egg JSON schema's exact field set (referenced through code
  but not directly read from an example egg).

These are recorded as INVESTIGATE in
`docs/audits/COMPONENT_DISPOSITION.md` and are not blockers for the
architecture phase.
