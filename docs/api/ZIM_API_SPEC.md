# ZIM API Specification (Phase 6)

This document specifies the ZIM API: the resource schemas, request
validation, response formats, structured errors, authentication,
permissions, pagination, filtering, concurrency controls, rate limits,
and the WebSocket event contract.

It is canonical for the API the engine exposes to the browser-side
integration layer and to ZIM extensions. The default frontend package
and any custom frontend package talk to this API; the engine's own
internal modules are not bound by this contract (they call each other
via Rust function calls).

## 1. Transport and base URL

- **HTTPS** for request/response operations. Base URL:
  `https://127.0.0.1:{engine_port}/api/v1`.
- **WSS** for streaming events. Base URL:
  `wss://127.0.0.1:{engine_port}/api/v1/stream`.
- **Loopback only.** The engine binds to `127.0.0.1` (or a
  Unix-domain socket on Linux/macOS, with an HTTPS-over-UDS fallback).
  Remote-node communication uses a separate, versioned contract
  (see `docs/architecture/RUST_ENGINE.md` §node-runtime).

## 2. Versioning

- The API is versioned by URL prefix (`/api/v1`, `/api/v2` in the
  future). The version is bumped on breaking changes; additive
  changes (new endpoints, new optional fields) do not bump the version.
- Each response includes the API version in the `X-ZIM-API-Version`
  response header (e.g. `v1.4.2` — major.minor.patch within v1).
- Clients send their requested version in the `Accept` header:
  `Accept: application/json; version=v1`.
- A client requesting a version the engine doesn't support receives
  `406 Not Acceptable` with a structured error including the
  supported versions.

## 3. Authentication

Three credential kinds:

- **User session** (cookie): issued by `POST /api/v1/auth/login`
  with email+password (or OAuth callback). The session cookie is
  `HttpOnly`, `Secure`, `SameSite=Strict`, scoped to the engine's
  loopback origin. Session lifetime is configurable; idle sessions
  are revoked after a configurable timeout.
- **API key** (bearer token): issued by `POST /api/v1/users/{id}/api-keys`
  (admin-only or self). Sent in `Authorization: Bearer {token}`.
  API keys have client-defined scopes (subset of `PermissionGrant`)
  and expiry.
- **Tab capability token** (header): issued by the engine to a tab
  when the tab navigates to a `zim://servers/{id}/...` URL. The
  token is scoped to that tab's permission set for that instance.
  Sent in `X-ZIM-Tab-Token: {token}`. The token is short-lived
  (refreshed by the integration layer on tab focus) and tied to a
  specific tab session.

No credential kind grants blanket access; every request is checked
against the specific permissions required by the operation.

## 4. Authorization

Each endpoint declares the `PermissionGrant` set it requires (see
`docs/architecture/RUST_ENGINE.md` §3 for the full enum). The
engine's authz module:

1. Resolves the caller's `PermissionSet` (from the credential).
2. Checks every required grant against the set.
3. If any required grant is missing, returns `403 Forbidden` with
   a structured error listing the missing grants.
4. If all grants are present, proceeds.

The check happens at the API boundary, in a single authz layer.
Domain services do not re-check permissions; the boundary is the
single enforcement point.

## 5. Request and response format

- **Request body:** `application/json` (UTF-8). Field names are
  `snake_case`.
- **Response body:** `application/json` (UTF-8). Field names are
  `snake_case`.
- **Date/time:** ISO 8601 UTC (e.g. `2026-10-13T15:46:09Z`).
- **Durations:** integer seconds or ISO 8601 duration (e.g. `PT30S`).
- **Bytes:** integer (e.g. `1610612736`).
- **IDs:** UUIDs (e.g. `550e8400-e29b-41d4-a716-446655440000`) for
  instances, nodes, eggs, jobs, backups, transfers, schedules,
  extensions; integer auto-increment for allocations and audit
  records.

## 6. Structured errors

Every error response is JSON:

```json
{
  "error": {
    "code": "permission_denied",
    "message": "The requested operation requires 'control.start' permission.",
    "missing_permissions": ["control.start"],
    "instance_id": "550e8400-e29b-41d4-a716-446655440000",
    "request_id": "req_01HJA...",
    "details": { /* optional, operation-specific */ }
  }
}
```

- `code` is a stable, machine-readable string from a documented
  vocabulary (see §6.1).
- `message` is human-readable, intended for display.
- `request_id` is a server-generated correlation ID; clients should
  log it for debugging.
- Other fields are operation-specific (e.g. `missing_permissions`
  for permission errors, `instance_id` for instance-scoped errors).

### 6.1 Error code vocabulary

| Code                          | HTTP | Meaning                                                                |
|--------------------------------|------|------------------------------------------------------------------------|
| `bad_request`                 | 400  | Malformed request body or query.                                       |
| `validation_failed`           | 400  | Request body is well-formed JSON but failed schema validation.          |
| `unauthorized`                | 401  | No valid credential; or credential expired.                            |
| `permission_denied`           | 403  | Credential valid but lacks required permissions.                       |
| `not_found`                   | 404  | Resource does not exist.                                               |
| `conflict`                     | 409  | Concurrent modification or state-machine violation.                    |
| `precondition_failed`         | 412  | Request precondition (e.g. `If-Match`) failed.                        |
| `payload_too_large`           | 413  | Request body exceeds limit.                                            |
| `unsupported_media_type`     | 415  | Wrong content type.                                                    |
| `rate_limited`                | 429  | Too many requests; see `Retry-After` header.                           |
| `internal_error`              | 500  | Unexpected server error.                                               |
| `not_implemented`             | 501  | Endpoint declared but not yet implemented (transitional).              |
| `service_unavailable`        | 503  | Engine is shutting down or a dependency is unavailable.                |
| `runtime_unavailable`         | 503  | The instance's runtime (Docker, etc.) is unreachable.                  |
| `instance_state_conflict`    | 409  | Operation cannot proceed given the instance's current state.          |
| `resource_exhausted`          | 507  | Disk/memory/allocation limit reached.                                  |
| `extension_manifest_invalid` | 400  | Extension package manifest failed validation.                          |
| `extension_signature_invalid`| 403  | Extension package signature is invalid or missing.                    |
| `frontend_smoke_test_failed`  | 422  | Frontend package failed the engine's activation smoke test.            |

## 7. Pagination, filtering, sorting

List endpoints accept:

- `page` (1-indexed, default 1) and `per_page` (default 50, max 200)
  OR `cursor` for cursor-based pagination (preferred for streams of
  records, e.g. audit log).
- `filter[<field>]` for filtering (e.g.
  `filter[state]=running,starting` for OR semantics on a single
  field).
- `sort` for sorting (e.g. `-created_at` for descending).

Responses include:

```json
{
  "data": [ /* items */ ],
  "pagination": {
    "page": 1,
    "per_page": 50,
    "total": 1342,
    "total_pages": 27
  }
}
```

or for cursor pagination:

```json
{
  "data": [ /* items */ ],
  "pagination": {
    "cursor": "eyJpZCI6MTIzNDV9",
    "next_cursor": "eyJpZCI6MTIzNDZ9",
    "has_more": true
  }
}
```

## 8. Concurrency control

- Mutating operations on a single instance require the client to
  send `If-Match: "<etag>"` with the instance's last-known ETag
  (returned in `ETag` response header on reads). A mismatch returns
  `412 Precondition Failed`.
- Long-running operations (backup, transfer, install) do not hold
  the HTTP request open. The client receives a job ID immediately
  and subscribes to the job's progress via the WebSocket stream.
- Per-instance operations are serialized by the engine's
  single-worker power queue; concurrent requests are queued, not
  rejected (with a configurable queue-depth limit; overflow returns
  `429 rate_limited` with `Retry-After`).

## 9. Rate limits

- Anonymous-style credential brute-force protection: per-IP rate
  limit on `/api/v1/auth/login` (e.g. 10 attempts / minute / IP).
- API key rate limits: per-key, configurable per key (default 600
  requests / minute).
- Tab capability token rate limits: per-tab, default 600 / minute.
- WebSocket subscriptions: per-tab, default 1 console stream + 1
  metrics stream + 1 events stream per tab; additional subscriptions
  return `429`.

`429` responses include `Retry-After` (seconds) and
`X-ZIM-RateLimit-Limit` / `X-ZIM-RateLimit-Remaining` /
`X-ZIM-RateLimit-Reset` headers (RFC standards).

## 10. Resources — endpoint inventory

### 10.1 Authentication

| Method | Path                          | Description                          | Required grants |
|--------|-------------------------------|--------------------------------------|------------------|
| POST   | `/api/v1/auth/login`         | Login with email+password.           | (none)           |
| POST   | `/api/v1/auth/logout`        | Logout (revoke session).             | (any session)    |
| POST   | `/api/v1/auth/refresh`       | Refresh session.                     | (any session)    |
| GET    | `/api/v1/auth/oauth/{provider}/start` | Start OAuth flow.          | (none)           |
| GET    | `/api/v1/auth/oauth/{provider}/callback` | OAuth callback.         | (none)           |

### 10.2 Users

| Method | Path                          | Description                          | Required grants |
|--------|-------------------------------|--------------------------------------|------------------|
| GET    | `/api/v1/users/me`            | Current user.                        | (any session)    |
| GET    | `/api/v1/users`               | List users.                          | admin            |
| POST   | `/api/v1/users`               | Create user.                         | admin            |
| GET    | `/api/v1/users/{id}`          | Get user.                            | self or admin    |
| PATCH  | `/api/v1/users/{id}`          | Update user.                         | self or admin    |
| DELETE | `/api/v1/users/{id}`          | Delete user.                         | admin            |
| GET    | `/api/v1/users/{id}/api-keys`| List user's API keys.                | self or admin    |
| POST   | `/api/v1/users/{id}/api-keys`| Create API key.                     | self or admin    |
| DELETE | `/api/v1/users/{id}/api-keys/{keyId}` | Revoke API key.            | self or admin    |

### 10.3 Instances

| Method | Path                                       | Description                          | Required grants |
|--------|--------------------------------------------|--------------------------------------|------------------|
| GET    | `/api/v1/instances`                        | List instances (filtered by user's perms). | (any session) |
| POST   | `/api/v1/instances`                        | Create instance.                     | admin            |
| GET    | `/api/v1/instances/{id}`                   | Get instance.                        | any instance grant |
| PATCH  | `/api/v1/instances/{id}`                   | Update instance config.              | AdminInstance or SettingsUpdate |
| DELETE | `/api/v1/instances/{id}`                   | Delete instance.                     | AdminInstance    |
| POST   | `/api/v1/instances/{id}/start`             | Start.                                | ControlStart     |
| POST   | `/api/v1/instances/{id}/stop`              | Stop.                                 | ControlStop      |
| POST   | `/api/v1/instances/{id}/restart`           | Restart.                              | ControlRestart   |
| POST   | `/api/v1/instances/{id}/kill`              | Force-kill.                           | ControlStop (elevated) |
| POST   | `/api/v1/instances/{id}/install`           | Install / reinstall.                  | Install          |
| POST   | `/api/v1/instances/{id}/suspend`           | Suspend (no auto-restart).            | AdminInstance    |
| POST   | `/api/v1/instances/{id}/transfer`          | Initiate transfer to another node.    | Transfer         |
| GET    | `/api/v1/instances/{id}/state`             | Observed state (small response).      | any instance grant |
| GET    | `/api/v1/instances/{id}/console`            | Console buffer (paginated).          | WebsocketConnect + ControlConsole (read) |
| POST   | `/api/v1/instances/{id}/console/input`     | Send console input.                  | ControlConsole   |
| GET    | `/api/v1/instances/{id}/metrics`            | Recent metrics (paginated).           | (any instance grant) |
| GET    | `/api/v1/instances/{id}/files`             | List files.                           | FileRead         |
| GET    | `/api/v1/instances/{id}/files/content`     | Get file content (download).          | FileReadContent  |
| PUT    | `/api/v1/instances/{id}/files/content`     | Write file content (upload).          | FileWrite        |
| POST   | `/api/v1/instances/{id}/files/create`      | Create file/dir.                      | FileCreate       |
| DELETE | `/api/v1/instances/{id}/files`             | Delete file/dir.                      | FileDelete       |
| POST   | `/api/v1/instances/{id}/files/archive`     | Archive (zip/tar).                    | FileArchive      |
| POST   | `/api/v1/instances/{id}/files/extract`     | Extract archive.                      | FileArchive + FileWrite |

### 10.4 Allocations

| Method | Path                                       | Description                          | Required grants |
|--------|--------------------------------------------|--------------------------------------|------------------|
| GET    | `/api/v1/instances/{id}/allocations`       | List instance's allocations.         | AllocationRead   |
| POST   | `/api/v1/instances/{id}/allocations`       | Assign new allocation.               | AllocationCreate |
| DELETE | `/api/v1/instances/{id}/allocations/{aid}` | Unassign allocation.                 | AllocationDelete |
| GET    | `/api/v1/nodes/{id}/allocations`           | List node's allocations.             | admin            |
| POST   | `/api/v1/nodes/{id}/allocations`            | Create allocation on node.           | admin            |

### 10.5 Backups

| Method | Path                                       | Description                          | Required grants |
|--------|--------------------------------------------|--------------------------------------|------------------|
| GET    | `/api/v1/instances/{id}/backups`           | List.                                 | BackupRead       |
| POST   | `/api/v1/instances/{id}/backups`           | Create (returns JobId).                | BackupCreate     |
| GET    | `/api/v1/instances/{id}/backups/{bid}`     | Get.                                  | BackupRead       |
| DELETE | `/api/v1/instances/{id}/backups/{bid}`     | Delete.                               | BackupDelete     |
| POST   | `/api/v1/instances/{id}/backups/{bid}/restore` | Restore (returns JobId).           | BackupRestore    |
| GET    | `/api/v1/instances/{id}/backups/{bid}/download` | Download backup bytes.         | BackupDownload   |
| GET    | `/api/v1/instances/{id}/backups/{bid}/lock` | Lock backup.                         | BackupUpdate     |
| DELETE | `/api/v1/instances/{id}/backups/{bid}/lock` | Unlock.                              | BackupUpdate     |

### 10.6 Schedules

| Method | Path                                       | Description                          | Required grants |
|--------|--------------------------------------------|--------------------------------------|------------------|
| GET    | `/api/v1/instances/{id}/schedules`         | List.                                 | ScheduleRead     |
| POST   | `/api/v1/instances/{id}/schedules`         | Create.                               | ScheduleCreate   |
| GET    | `/api/v1/instances/{id}/schedules/{sid}`   | Get.                                  | ScheduleRead     |
| PATCH  | `/api/v1/instances/{id}/schedules/{sid}`   | Update.                               | ScheduleUpdate   |
| DELETE | `/api/v1/instances/{id}/schedules/{sid}`   | Delete.                               | ScheduleDelete   |
| POST   | `/api/v1/instances/{id}/schedules/{sid}/run` | Trigger now.                       | ScheduleUpdate   |

### 10.7 Subusers

| Method | Path                                       | Description                          | Required grants |
|--------|--------------------------------------------|--------------------------------------|------------------|
| GET    | `/api/v1/instances/{id}/subusers`          | List.                                 | UserRead         |
| POST   | `/api/v1/instances/{id}/subusers`          | Add.                                  | UserCreate       |
| PATCH  | `/api/v1/instances/{id}/subusers/{uid}`   | Update permissions.                  | UserUpdate       |
| DELETE | `/api/v1/instances/{id}/subusers/{uid}`   | Remove.                               | UserDelete       |

### 10.8 Jobs

| Method | Path                                       | Description                          | Required grants |
|--------|--------------------------------------------|--------------------------------------|------------------|
| GET    | `/api/v1/jobs`                              | List jobs (filtered).                | admin (or own)  |
| GET    | `/api/v1/jobs/{jid}`                       | Get job status.                      | admin (or own)  |
| POST   | `/api/v1/jobs/{jid}/cancel`                | Cancel.                              | admin (or own)  |

### 10.9 Transfers

| Method | Path                                       | Description                          | Required grants |
|--------|--------------------------------------------|--------------------------------------|------------------|
| GET    | `/api/v1/instances/{id}/transfer`          | Current transfer status.             | Transfer         |
| POST   | `/api/v1/instances/{id}/transfer`         | Initiate transfer.                   | Transfer         |
| DELETE | `/api/v1/instances/{id}/transfer`         | Cancel transfer.                     | Transfer         |

### 10.10 Nodes

| Method | Path                          | Description                          | Required grants |
|--------|-------------------------------|--------------------------------------|------------------|
| GET    | `/api/v1/nodes`               | List nodes.                          | admin            |
| POST   | `/api/v1/nodes`               | Create node.                        | admin            |
| GET    | `/api/v1/nodes/{id}`          | Get node.                            | admin            |
| PATCH  | `/api/v1/nodes/{id}`          | Update node.                        | admin            |
| DELETE | `/api/v1/nodes/{id}`          | Delete node (must be empty).         | admin            |
| GET    | `/api/v1/nodes/{id}/state`    | Node state + heartbeat.              | admin            |

### 10.11 Eggs (server definitions)

| Method | Path                          | Description                          | Required grants |
|--------|-------------------------------|--------------------------------------|------------------|
| GET    | `/api/v1/eggs`                | List eggs.                          | (any session)    |
| POST   | `/api/v1/eggs`                | Import egg (legacy egg JSON or zim.toml). | admin       |
| GET    | `/api/v1/eggs/{id}`           | Get egg.                            | (any session)    |
| PATCH  | `/api/v1/eggs/{id}`           | Update egg.                         | admin            |
| DELETE | `/api/v1/eggs/{id}`           | Delete egg.                         | admin            |

### 10.12 Extensions

| Method | Path                          | Description                          | Required grants |
|--------|-------------------------------|--------------------------------------|------------------|
| GET    | `/api/v1/extensions`         | List installed.                     | (any session)    |
| POST   | `/api/v1/extensions`         | Install (upload package).           | admin            |
| DELETE | `/api/v1/extensions/{id}`    | Uninstall.                          | admin            |
| POST   | `/api/v1/extensions/{id}/enable`  | Enable.                        | admin            |
| POST   | `/api/v1/extensions/{id}/disable` | Disable.                       | admin            |
| POST   | `/api/v1/extensions/{id}/grant`   | Grant additional permissions.  | admin            |

### 10.13 Audit

| Method | Path                          | Description                          | Required grants |
|--------|-------------------------------|--------------------------------------|------------------|
| GET    | `/api/v1/audit`               | Query audit log (cursor paginated).  | admin            |

### 10.14 Frontend packages

| Method | Path                          | Description                          | Required grants |
|--------|-------------------------------|--------------------------------------|------------------|
| GET    | `/api/v1/frontend/active`    | Get active package info.             | (any session)    |
| POST   | `/api/v1/frontend/active`     | Set active package.                  | admin            |
| POST   | `/api/v1/frontend/upload`     | Upload new package.                  | admin            |
| POST   | `/api/v1/frontend/smoke-test` | Run smoke test on a candidate package. | admin          |
| POST   | `/api/v1/frontend/reset`      | Reset to default.                    | admin            |

### 10.15 Workloads (canonical, per AD-19)

The `/api/v1/workloads` surface is the canonical resource for managed
workloads. The legacy `/api/v1/instances` surface (§10.3) is preserved
as a back-compat alias that maps to the same handlers; new integrations
and new user-facing documentation use the `workloads` form.

| Method | Path                                          | Description                                                | Required grants |
|--------|-----------------------------------------------|------------------------------------------------------------|------------------|
| GET    | `/api/v1/workloads`                            | List workloads (filtered by user's perms, by kind).       | (any session)    |
| POST   | `/api/v1/workloads`                            | Create workload (kind required; spec is kind-specific).    | `workload.create` or `workload.create.{kind}` |
| GET    | `/api/v1/workloads/{id}`                       | Get workload (kind + spec + state).                       | any workload grant for this id |
| PATCH  | `/api/v1/workloads/{id}`                       | Update workload spec (kind-specific fields).              | `workload.update` or admin |
| DELETE | `/api/v1/workloads/{id}`                       | Delete workload.                                            | `workload.delete` or admin |
| GET    | `/api/v1/workloads/{id}/state`                 | Observed state + readiness (if `ReadyProbeCapable`).      | any workload grant |
| POST   | `/api/v1/workloads/{id}/start`                 | Start.                                                      | `control.start` |
| POST   | `/api/v1/workloads/{id}/stop`                  | Stop (graceful; runtime-specific semantics).               | `control.stop` |
| POST   | `/api/v1/workloads/{id}/restart`               | Restart.                                                    | `control.restart` |
| POST   | `/api/v1/workloads/{id}/kill`                  | Force-kill.                                                 | `control.stop` (elevated) |
| POST   | `/api/v1/workloads/{id}/install`               | Install / reinstall.                                        | `install` |
| POST   | `/api/v1/workloads/{id}/migrate`               | Initiate migration to another node (returns JobId).        | `workload.migrate` |
| GET    | `/api/v1/workloads/{id}/capabilities`         | List the runtime's declared capabilities for this workload. | any workload grant |
| POST   | `/api/v1/workloads/{id}/console/input`         | Send console input (returns 409 `unsupported` if runtime is not `ConsoleCapable`). | `control.console` |
| GET    | `/api/v1/workloads/{id}/network/endpoints`     | List network endpoints (409 `unsupported` if not `NetworkCapable`). | `network.read` or any workload grant |
| POST   | `/api/v1/workloads/{id}/network/endpoints`     | Expose a new port (409 `unsupported` if not `NetworkCapable`). | `network.expose` |
| DELETE | `/api/v1/workloads/{id}/network/endpoints/{eid}` | Unexpose a port.                                         | `network.unexpose` |
| POST   | `/api/v1/workloads/{id}/deploy`                | Deploy a new version (Website workloads only; 409 otherwise). | `workload.deploy` |
| POST   | `/api/v1/workloads/{id}/snapshot`              | Request a snapshot (returns JobId; 409 if not `SnapshotCapable`). | `workload.snapshot` |

### 10.16 Workload integrations (admin surface)

| Method | Path                                                | Description                                                | Required grants |
|--------|------------------------------------------------------|------------------------------------------------------------|------------------|
| GET    | `/api/v1/workload-integrations`                       | List installed integrations (first-party + third-party).   | admin            |
| POST   | `/api/v1/workload-integrations`                       | Upload a third-party integration package.                   | admin            |
| GET    | `/api/v1/workload-integrations/{id}`                  | Get integration info (manifest + capabilities + kinds).    | admin            |
| POST   | `/api/v1/workload-integrations/{id}/enable`           | Enable.                                                    | admin            |
| POST   | `/api/v1/workload-integrations/{id}/disable`          | Disable (existing workloads of this kind continue but cannot be re-created). | admin            |
| DELETE | `/api/v1/workload-integrations/{id}`                  | Uninstall (blocked if any workload of this kind exists).   | admin            |

### 10.17 Back-compat aliasing rules

The `/api/v1/instances` surface (§10.3) is preserved verbatim and
internally aliased to `/api/v1/workloads`:

- A workload of kind `GameServer` corresponds 1:1 to an Instance row.
- All `/api/v1/instances/{id}/...` paths return the same responses
  as `/api/v1/workloads/{id}/...` for the same id.
- `/api/v1/instances` does not expose workload-kind-specific
  endpoints (e.g. `/console/input` is present; `/network/endpoints`
  is not — game servers have allocations, not multi-endpoint
  networking).
- New endpoints introduced for non-game-server workload kinds are
  available only on the `/api/v1/workloads` surface.

This satisfies AD-19 (workloads canonical, instances back-compat)
without breaking existing client code that uses `/api/v1/instances`.

## 11. WebSocket event contract

WSS endpoint: `wss://127.0.0.1:{port}/api/v1/stream?instance_id={id}`.

- Auth: `Authorization: Bearer {token}` header on the WSS upgrade
  (or `X-ZIM-Tab-Token` for tab tokens).
- Sub-protocols: `zim-events.v1` (the only one currently supported).
- Replay-from-checkpoint: client sends `?since_generation={n}` on
  connect; engine replays events with `generation > n` then
  live-streams.

### 11.1 Event envelope

```json
{
  "type": "instance.state_change" | "instance.console_output" | "instance.resource_usage" | "instance.install_progress" | "job.progress" | "job.completed" | "node.state_change" | "audit.record",
  "generation": 12345,
  "ts": "2026-10-13T15:46:09.123Z",
  "instance_id": "550e8400-e29b-41d4-a716-446655440000",
  "data": { /* event-specific */ }
}
```

### 11.2 Event ordering and delivery

- Events for the same `instance_id` are delivered in
  `generation` order.
- Events across instances are NOT strictly ordered; subscribers must
  not assume cross-instance ordering.
- Duplicate events (same `generation` on the same stream) are
  suppressed by the engine.
- Backpressure: if the subscriber's queue is full, the engine drops
  the oldest event and increments the subscriber's `dropped_events`
  counter; the engine emits a `stream.dropped` meta-event on the
  next available slot.

### 11.3 Reconnect and replay

On reconnect with `?since_generation={n}`:

- The engine replays events with `generation > n` for the
  subscriber's filter.
- Events older than the engine's retention window (configurable; for
  console output, default 1MB or 10k events per instance; for state
  changes, all since instance creation) are not replayable; the
  engine emits a `stream.gap` event instead, signaling the
  subscriber should re-fetch the current state via the HTTP API.

## 12. Idempotency

- All mutating operations accept an optional `Idempotency-Key`
  header (RFC draft). The engine stores the response for the key
  for 24 hours; a repeat request with the same key returns the
  stored response.
- This is critical for unreliable-network clients (e.g. the browser
  re-sending a start request after a transient network blip).

## 13. Limits

| Limit                          | Default           | Configurable |
|--------------------------------|-------------------|--------------|
| Request body size             | 10 MB (file uploads: 5 GB) | Yes          |
| Console input line length     | 64 KB             | Yes          |
| Audit log retention           | 365 days           | Yes          |
| Console buffer per instance   | 1 MB / 10k events | Yes          |
| WebSocket subscribers per tab | 3                 | Yes          |
| WebSocket subscribers per instance | 50            | Yes          |

## 14. What the API does NOT expose

- Direct database access. The browser has no DB driver; it only
  talks to the API.
- Arbitrary shell commands. No `POST /exec` for arbitrary commands;
  console input is sent via `/console/input` and is validated
  against `ControlConsole` permission.
- Implementation-specific Rust types. The API is JSON; it does not
  leak Rust enums' discriminants or internal struct layouts.
- Filesystem paths outside an instance's data directory. The file
  API is scoped to the instance's directory; path traversal is
  blocked (see `docs/security/THREAT_MODEL.md`).
- Engine-internal module boundaries. The API is organized by
  resource, not by Rust crate.

## 15. Open API questions

1. The exact pagination cursor encoding (opaque base64 vs. explicit
   `after_id`) is open; preliminary: opaque base64 to allow
   schema changes.
2. The exact OAuth provider set (Google, GitHub, Discord, etc.) is
   open; preliminary: pluggable via a config file, with Discord
   first (matches the gaming audience).
3. The exact rate-limit window granularity is open; preliminary:
   sliding window per credential.
