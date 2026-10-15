# Migration & Compatibility (Phase 6)

This document specifies how ZIM supports migration from existing
Pterodactyl deployments: the data import path, the legacy egg
compatibility surface, the legacy client API compatibility surface
(deferred), and the documented behavioral differences.

It is canonical for migration and compatibility; the audit
documents (`docs/audits/PTERODACTYL_ARCHITECTURE.md`) are canonical
for the upstream behavior being migrated from.

## 1. Migration paths

ZIM supports three migration paths from Pterodactyl:

1. **Panel-database migration** — read an existing Pterodactyl
   Panel MySQL database and produce a ZIM engine SQLite (or
   Postgres) database with the users, instances, nodes, allocations,
   eggs, schedules, subusers, and settings mapped to ZIM's schema.
2. **Egg import** — import existing Pterodactyl eggs (JSON files)
   into ZIM's egg store, with the legacy format preserved and a
   ZIM-native `zim.toml` format preferred for new eggs.
3. **Filesystem migration** — copy an existing server's data
   directory (from a Wings-managed `/var/lib/pterodactyl/volumes/`
   or equivalent) into a ZIM-managed data directory, preserving
   the instance's files, configuration, and (where applicable)
   the in-container state.

Each path is documented below.

## 2. Panel-database migration

### 2.1 Source

The importer reads an existing Pterodactyl Panel MySQL database
(the schema documented in `upstream/pterodactyl-panel/database/migrations/`,
195 migrations from 2016-01-23 to 2024-07-13, verified at the
pinned Panel revision v1.15.1).

### 2.2 Destination

The importer produces a ZIM SQLite (default) or Postgres database
with the schema documented in `docs/architecture/RUST_ENGINE.md`
§persistence.

### 2.3 Table mapping

| Panel table (MySQL)    | ZIM table (SQLite/Postgres) | Notes |
|------------------------|------------------------------|-------|
| `users`                | `users`                       | `external_id` preserved for round-trip; password hashes re-hashed with Argon2id (the Panel's bcrypt hashes are re-hashed on first login with a documented migration path; users are not required to reset passwords). |
| `servers`              | `instances`                   | All `@property` fields from `app/Models/Server.php` preserved (see `docs/audits/PTERODACTYL_ARCHITECTURE.md` §7.1.3). `status` mapped from string to `InstanceStatus` enum (legacy `starting`/`running`/`stopping`/`offline`/`crashed`/`installing`/`suspended`/`restoring_backup`/`transferring` map to the new enum). |
| `allocations`          | `allocations`                 | Direct copy. |
| `nodes`                | `nodes`                       | `deployment` set to `RemoteLinux` (the Panel's nodes are remote Linux daemons). `last_heartbeat` set to null on import (will be set when the node reconnects to ZIM's engine). |
| `locations`            | `locations`                   | Direct copy. |
| `nests`                | `nests`                       | Direct copy (kept for legacy compat; ZIM's native egg store does not require nests). |
| `eggs`                 | `eggs`                        | Imported via the egg importer (§3). |
| `eggs_variables`       | `egg_variables`               | Direct copy. |
| `server_variables`     | `instance_variables`           | Direct copy. |
| `subusers`             | `subusers`                    | Direct copy. |
| `permissions`          | `permissions`                  | Permission names mapped from Panel's string constants to ZIM's `PermissionGrant` enum (vocabulary preserved verbatim). |
| `schedules`           | `schedules`                    | Direct copy. |
| `schedule_tasks`      | `schedule_tasks`              | Direct copy. |
| `databases`            | `databases`                    | Direct copy (databases are a deferred ZIM feature; the rows are imported but no engine functionality uses them initially). |
| `database_hosts`      | `database_hosts`              | Direct copy. |
| `mounts`              | `mounts`                       | Direct copy. |
| `mount_node`          | `mount_node`                   | Direct copy. |
| `mount_server`         | `mount_server`                 | Direct copy. |
| `backups`             | `backups`                      | Direct copy; `is_successful` mapped to `state` (Succeeded if true, Failed otherwise). |
| `api_keys`            | `api_keys`                     | Direct copy; `scopes` parsed from Panel's `abilities` JSON to ZIM's `PermissionSet`. |
| `activity_logs`       | `audit`                        | Direct copy; `event` mapped to ZIM's `action` field. |
| `settings` (key/value)| `engine_settings`              | Each known key mapped to its ZIM equivalent. Unknown keys are preserved in `engine_settings_unknown` for inspection. |
| `server_transfers`    | `transfers`                    | Direct copy. |

### 2.4 Process

1. The user runs `zim migrate pterodactyl --panel-mysql-dsn=... --zim-db=/path/to/zim.sqlite`.
2. The importer connects to the Panel MySQL database read-only.
3. For each table in the mapping, the importer reads the rows and
   writes to the ZIM SQLite database in a single transaction per
   table (or batched if the table is large).
4. The importer prints a summary: rows imported, rows skipped (with
   reasons), and warnings (e.g. unknown `settings` keys).
5. The user starts the ZIM engine pointing at the new ZIM DB.
6. On first startup, the engine reconciles persisted state with
   runtime observed state for every instance (per the lifecycle
   invariants in `docs/architecture/RUST_ENGINE.md` §lifecycle).
   Instances that the runtime reports as not running are placed
   in the appropriate state (Offline / Crashed / Unknown).

### 2.5 Validation

- Each imported row is validated against ZIM's schema. Invalid rows
  are skipped with a documented reason in the import summary.
- The importer does not modify the source Panel database (read-only
  connection).
- A dry-run mode (`--dry-run`) prints what would be imported without
  writing to the ZIM DB.

### 2.6 Limitations

- The Panel's `activity_logs` table may have non-JSON-serializable
  metadata in old rows; these rows are imported with the metadata
  stored as a string fallback, not parsed.
- The Panel's `api_keys` `abilities` field may include permission
  names ZIM doesn't recognize; these are imported as unknown grants
  (no effect) and surfaced in the import summary.
- The Panel's `eggs` `config` field uses Wings-specific parser
  rules; ZIM's importer preserves the config but the runtime
  parser may produce slightly different output for edge-case
  formats. The differences are documented in §3.4.

## 3. Egg import

### 3.1 Source format

The Pterodactyl egg format is a JSON document with fields including
`name`, `description`, `author`, `docker_image`, `startup`,
`config` (parser rules), `scripts.install` (Bash script),
`scripts.update`, `variables` (env-var declarations), `features`
(feature flags). The format is community-documented at the
`pterodactyl/eggs` GitHub repository (not in the ZIM repo).

### 3.2 Destination format

ZIM's native egg format is a `zim.toml` file with a typed schema
(variables are typed; install scripts are explicitly scoped;
permissions are declared). The native format is preferred for new
eggs but the legacy JSON format is fully supported via the importer.

### 3.3 Import process

1. The user runs `zim eggs import /path/to/egg.json` or uploads via
   `POST /api/v1/eggs`.
2. The importer parses the JSON, validates the schema, and converts
   to ZIM's internal egg representation.
3. The install script is stored verbatim; ZIM does not modify it.
4. The startup command template is preserved with its `{{...}}`
   placeholders; the runtime substitutes variables at startup.
5. The config parser rules are translated to ZIM's runtime
   config-rewriting module (which uses the same `beevik/etree`
   XML parsing approach as Wings for compatibility).

### 3.4 Behavioral compatibility

Most Pterodactyl eggs work in ZIM without modification. Documented
differences:

- **Privileged install scripts:** Eggs with
  `features.priviledged_install_scripts: true` are rejected by
  default and require explicit admin opt-in per egg.
- **Wings-specific output parsing:** Eggs that depend on Wings's
  exact output-parser behavior may have slightly different console
  output in ZIM (e.g. ANSI escape sequences are passed through
  verbatim in ZIM; Wings may strip some). Where this matters, the
  egg author should update the parser rules to match ZIM's
  behavior.
- **Startup time environment variable substitution:** ZIM preserves
  the `{{SERVER_MEMORY}}`, `{{SERVER_IP}}`, `{{SERVER_PORT}}`,
  `{{ENV_VARIABLE}}` placeholder vocabulary. Eggs using these
  placeholders work unmodified.
- **Container user:** ZIM runs containers as a non-root user by
  default; eggs that require root must declare
  `features.requires_root: true` and are rejected by default.

### 3.5 Validation

- Schema validation: the importer validates the egg JSON against a
  JSON Schema derived from Pterodactyl's documented egg format.
- Install-script static analysis: the importer runs a static
  analyzer on the install script (a Bash parser; in the
  implementation phase, `shellcheck` is integrated) and warns on
  risky patterns (e.g. `curl | sh`, unquoted variables in paths).
- Startup command validation: the importer parses the startup
  template and reports missing variable references.

### 3.6 Limitations

- The Pterodactyl egg format is community-evolved; some eggs use
  undocumented features. ZIM's importer surfaces these as warnings
  and imports the egg with the unknown features ignored.
- ZIM does not run the Pterodactyl `eggs` repository's tests; the
  egg is imported as-is. Validation is the egg author's
  responsibility (and the admin's review).

## 4. Filesystem migration

### 4.1 Source

An existing Pterodactyl Wings installation stores server files
under (default) `/var/lib/pterodactyl/volumes/{server_uuid}/` on
the Wings host. Each server's data directory contains the server's
files (mounted into the container at `/home/container`).

### 4.2 Destination

ZIM stores server files under (default)
`~/.local/share/zim/instances/{instance_id}/data/` on the engine
host (or, for remote nodes, under the node's own data directory).

### 4.3 Process

1. Stop the server in Pterodactyl.
2. Copy the server's data directory to the ZIM engine's (or remote
   node's) data directory, renaming from `{server_uuid}` to ZIM's
   `{instance_id}` (the Panel-database migration preserves the
   `uuid` field; ZIM uses it as the `instance_id` for migrated
   instances to simplify this step).
3. Set the data directory's ownership to the user that runs the
   ZIM engine (or the user that runs the node-runtime on a remote
   node).
4. Start the instance in ZIM. The runtime (Docker) mounts the
   data directory into the container at `/home/container` (matches
   Wings).

### 4.4 Limitations

- The data directory must be on a filesystem that supports the
  file capabilities ZIM needs (extended attributes for quarantine
  markers; default Linux ext4 / XFS are fine).
- File ownership in the data directory may need to be adjusted to
  match ZIM's container user (UID 988 by default; configurable).
- A large data directory may take significant time to copy;
  `rsync` is the recommended tool.

## 5. Legacy client API compatibility (DEFERRED)

The Panel exposes a client API surface
(`upstream/pterodactyl-panel/routes/api-client.php`) that some
third-party tools (Pterodactyl-compatible mobile apps, dashboards,
billing integrations) consume. ZIM may preserve this surface for
legacy compatibility; this is **deferred** to a later milestone.

When implemented, the legacy client API will:

- Be a separate set of routes (`/api/legacy/client/v1/...`) that
  maps to the same engine operations as the native API.
- Preserve the Panel's response formats (including its pagination,
  error format, and field naming) for tool compatibility.
- Be opt-in (admin enables it per deployment; default off).

## 6. Documented behavioral differences

Even with the migration paths above, ZIM is not a drop-in
replacement for Pterodactyl. The documented differences:

| Behavior                                | Pterodactyl                                 | ZIM                                              |
|-----------------------------------------|---------------------------------------------|--------------------------------------------------|
| Frontend                                | Web panel served by PHP/Laravel              | Chrome-inspired desktop shell (zim:// pages)               |
| Backend                                  | Panel (PHP) + Wings (Go)                     | Single Rust engine                                |
| Server lifecycle authority              | Wings (the daemon)                         | The engine (a single process)                    |
| Console stream protocol                  | Custom WS protocol between Wings and Panel    | Documented WS event contract with replay-from-checkpoint |
| File API                                 | Wings HTTP API                              | Engine HTTP API with security-tight archive limits |
| Backup storage                           | Wings-local or S3-compatible                 | Engine-local or S3-compatible (same)              |
| Permission vocabulary                    | Panel `Permission` constants                 | Same vocabulary, mapped to `PermissionGrant` enum |
| Power-action vocabulary                  | `start`, `stop`, `restart`, `kill`         | Same                                              |
| Process states                           | `offline`, `starting`, `running`, `stopping` | Same, plus `crash_loop`, `unknown` (ZIM extensions) |
| Egg install script runtime              | Wings (Docker container)                    | Engine (Docker container; same model)            |
| Schedule execution                       | Wings (gocron)                              | Engine (scheduler module; same semantics)        |
| Audit log                                | Panel activity_logs                         | Engine audit (append-only; same vocabulary)       |
| Multi-node coordination                  | Panel↔Wings split                            | Engine + remote-node contract (single backend)   |
| Authentication                            | Panel sessions + JWT for Wings              | Engine sessions + tab tokens + API keys          |
| SFTP                                     | Wings SFTP server                           | Engine SFTP (deferred; not in first release)      |

These differences are documented in
`docs/api/ZIM_API_SPEC.md` (the new contracts) and surfaced to
the user during the migration process.

## 7. Migration E2E test

A migration E2E test is identified in
`docs/architecture/TEST_STRATEGY.md`:

1. Start a Pterodactyl Panel + Wings deployment (using Docker
   fixtures; the test environment must include Docker).
2. Create a fixture instance with a known egg (e.g. vanilla
   Minecraft).
3. Run the ZIM migration importer.
4. Start the ZIM engine.
5. Verify the instance is present, its state is correct, and
   its files are accessible via the ZIM file API.
6. Start the instance in ZIM; verify it runs.
7. Stop the instance; verify it stops cleanly.

This test is the canonical migration acceptance test; passing it
is the gate for "migration from Pterodactyl is supported".

## 8. Open compatibility questions

1. Whether to preserve the Panel's `api-application` surface (for
   third-party billing integrations) in addition to `api-client`.
   Preliminary: yes, deferred.
2. Whether to preserve the Panel's WebSocket protocol for
   console streaming (for tools that consume it directly, bypassing
   the Panel). Preliminary: no, document the breaking change.
3. Whether the legacy client API should be a separate ZIM
   extension package (so it can be enabled/disabled per
   deployment) or built into the engine. Preliminary: separate
   extension package.
