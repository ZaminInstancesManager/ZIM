# Migrating from Pterodactyl to ZIM — the practical guide

You run a Pterodactyl Panel (and Wings nodes) today. This guide
walks you onto ZIM: exporting your panel's data, importing it into
a fresh ZIM engine, moving your servers' files, and verifying
everything came across. The normative specification lives in
`docs/compatibility/MIGRATION_AND_COMPATIBILITY.md`; this document
is the operator's companion — commands, order, and what to check.

**Time budget:** an hour for a small panel (a dozen servers), an
afternoon for a large one (filesystem copies dominate).

## 0. What moves and what doesn't

| Pterodactyl | ZIM | Notes |
|---|---|---|
| Panel users (incl. 2FA flags, OAuth subjects) | ZIM users | Passwords **do not** migrate (different hashing policy) — users reset on first login. |
| Servers | Instances | Names, descriptions, limits, allocations, docker images, startup commands. |
| Nodes | The local node | ZIM v0.1 manages one node (the machine the engine runs on). Remote-node topology arrives post-alpha; every server must live on the ZIM host. |
| Allocations | Port/ip assignments | Per-node free pools, one primary + extras per instance. |
| Eggs | ZIM eggs | The legacy JSON egg format imports as-is; ZIM-native `zim.toml` eggs are preferred for new work. |
| Schedules | ZIM schedules | Cron expressions, `only_when_online`, power actions. |
| Subusers | Subuser grants | Permission sets map onto ZIM's grant model. |
| Server files | Instance data dirs | You copy them (§3). Nothing reads live from your Wings volumes. |
| SFTP keys / legacy client API | — | Not in this release (see the alpha notes). |

## 1. Prerequisites

- The ZIM engine binary (from the release, or built from source:
  `cd zim/engine && cargo build --release -p zim_engine_main`).
- A Docker daemon on the ZIM host (lifecycle operations need it).
- **Read access to your Panel's MySQL database** — either a DSN
  (`mysql://user:pass@host:3306/panel`) or a fixture export.
- A maintenance window: migrate servers one at a time, stopped.

### Exporting a fixture (when you'd rather not point ZIM at production MySQL)

A fixture is a single JSON document shaped like the panel's tables.
The importer accepts it via `--panel-fixture=PATH`. The reference
shape lives at
`zim/engine/crates/migrate-pterodactyl/tests/fixtures/panel_fixture.json`
(users, servers, nodes, allocations, eggs, schedules, subusers as
JSON arrays keyed the way the Panel schema names them). Any tool
that dumps those tables to JSON in that shape works — e.g. a small
`mysqldump` + `jq` pass on a *read replica*.

## 2. Import the panel database

Always rehearse first — dry-run reads the source, maps everything,
and prints a report **without writing**:

```bash
zim_engine migrate pterodactyl \
  --panel-mysql-dsn='mysql://readonly:pass@127.0.0.1:3306/panel' \
  --zim-db=/var/lib/zim/engine.db \
  --dry-run
```

Read the report: it names every table it read, every row it
mapped, and every row it refused (with reasons — see the policy
flags below). When the numbers look right, run it for real (drop
`--dry-run`). The importer writes a fresh ZIM SQLite database; it
never touches the Panel's MySQL.

Two refusals you might see, and their escape hatches:

- **Privileged install scripts** (eggs whose install script runs as
  root): refused by default. Re-enable deliberately with
  `--allow-privileged-scripts`.
- **Eggs marked requires-root**: same idea, `--allow-root`.

Using a fixture instead of a DSN: swap
`--panel-mysql-dsn=…` for `--panel-fixture=panel_fixture.json`.

### First boot on the migrated database

```bash
zim_engine --data-dir /var/lib/zim
```

The engine picks up the database you imported (the data dir's
`engine.db`), reconciles its instance table against reality (no
containers running yet — every instance comes up `offline`, which
is ZIM's word for stopped), and serves the UI at the loopback
address it prints. The desktop shell attaches to this engine
automatically. Administrator accounts migrate as ordinary admins;
bootstrap a fresh root with
`ZIM_BOOTSTRAP_ADMIN_EMAIL=… ZIM_BOOTSTRAP_ADMIN_PASSWORD=…` on
first boot if you're locked out.

## 3. Move each server's files

For each migrated instance, copy its data directory from the Wings
host into the ZIM engine's instance directory:

```bash
# On the Wings host: stop the server, then pack its volume
sudo tar -C /var/lib/pterodactyl/volumes/<uuid> -czf /tmp/<uuid>.tar.gz .

# On the ZIM host: unpack into the instance's data dir
# (the engine's data dir + instances/<uuid-short>; create it)
sudo mkdir -p /var/lib/zim/instances/<uuid-short>
sudo tar -C /var/lib/zim/instances/<uuid-short> -xzf /tmp/<uuid>.tar.gz
```

The import report lists each instance's `uuid-short` (the
directory name ZIM expects). File ownership should match the
container user the runtime uses (uid 988 by default, Wings parity).
The instance's egg, image, startup command, allocations, and
limits already migrated with the database — the files are the only
thing that moves by hand.

## 4. Verify, server by server

For each migrated instance, in the ZIM UI (or via the API):

1. **Inspect**: the console page shows the instance's name, egg,
   state `offline`, and its allocations — all carried from the
   Panel.
2. **Start**: the container boots with the migrated image and
   startup command; the console streams.
3. **Stop**: graceful stop converges back to `offline`.
4. **Schedules**: listed with their cron; run-now fires them.
5. **Backups**: take one — the archive seals with a checksum.

If a server misbehaves, check the engine's stderr (the shell
writes it under the data dir) — start failures print their reason
there, verbatim.

## 5. Rollback

Nothing in this migration writes to your Pterodactyl Panel. To
roll back: stop the ZIM engine, point Wings back at the (still
untouched) volumes, and start your Panel as before. The ZIM data
dir is self-contained and can simply be deleted.

## 6. Where to read more

- The normative mapping tables and behavioral differences:
  `docs/compatibility/MIGRATION_AND_COMPATIBILITY.md`.
- The API you're migrating onto: `docs/api/ZIM_API_SPEC.md`.
- Egg import specifics and the ZIM-native egg format:
  `docs/architecture/RUST_ENGINE.md` (egg importer sections).
- What this alpha does and doesn't do:
  `docs/releases/ALPHA_v0.1.0.md`.
