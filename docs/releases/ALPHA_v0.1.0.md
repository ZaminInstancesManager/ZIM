# ZIM Alpha Release Notes — v0.1.0-alpha.1

> **This is an alpha.** ZIM v0.1.0-alpha.1 is the first usable
> release of the Zamin Instances Manager: real, installable,
> end-to-end tested — and explicitly **not production-ready**.
> Expect rough edges; report what breaks.

## What ZIM is

ZIM is an independent desktop product of the ZaminMC ecosystem: a
Chrome-inspired desktop application powered by a Rust-based
workload-management engine. You manage long-running workloads —
game servers, databases, websites, development processes, generic
containers — through a tabbed, omnibox-driven interface, while the
engine runs as an independent, separately-recoverable authority on
your machine. Minecraft is the flagship workload; Pterodactyl
deployments can be migrated in.

## What works (and how we know)

Everything below is exercised by real end-to-end runs on every
push: the actual engine binary, the actual embedded frontend, real
TLS, a real headless Chromium, and real Docker containers on the
Linux leg of the matrix.

- **Install & first boot** — the desktop shell attaches to a
  running engine or spawns a detached one (closing the window never
  stops your servers); first boot bootstraps the administrator and
  the local node; the engine's loopback certificate is installed
  into the OS trust store.
- **The application chrome** — the Chrome-style tab strip, omnibox,
  split view, tear-off/merge multi-window, live theme system with
  presets and packaged themes, and the new-tab page with
  engine-backed recents, create/import flows, and customization.
- **The full instance lifecycle** (Docker hosts) — create, start,
  view a live console, stop, restart; the power queue serializes
  operations; crash-loop protection and engine-restart
  reconciliation re-adopt running containers instead of
  duplicating them; tabs are views, never processes (closing a tab
  never stops its server).
- **The live event stream** — the `zim-events.v1` WebSocket with
  replay-from-checkpoint and bounded backpressure drives live
  console output and live status everywhere. (This release
  contains the fix for a defect the E2E suite caught in the wild:
  the production TLS accept loop was dropping every WebSocket
  subscriber on arrival — every live surface silently fell back to
  buffered output. Fixed, with a regression test over the real TLS
  loop.)
- **Backups** — create, seal (sha256 + size), lock, restore,
  delete, with honest failed-job states.
- **Schedules** — cron schedules with validation, pause, run-now,
  and the `only_when_online` gate.
- **Eggs & installs** — Pterodactyl egg import and the constrained
  container install pipeline.
- **Five workload kinds** — game servers (Minecraft at egg parity),
  databases (Postgres / MariaDB / MySQL / Redis with readiness
  probes and quiesced snapshot/restore), websites (static nginx
  carrier with release-directory deploys and health checks),
  development processes (file-watch restart supervision), and
  generic containers (placement-time capability negotiation).
- **Extensions & themes** — the `zim.addon.json` package format,
  ed25519-signed packages, the install/enable/disable/uninstall
  lifecycle with surface-collision refusal, packaged themes
  validated against the `theme-tokens.v1` vocabulary, and the
  bundled audit-log viewer addon.
- **Pterodactyl migration** — panel-database import (dry-run and
  real), egg import, filesystem migration. See
  `docs/compatibility/PTERODACTYL_MIGRATION_GUIDE.md`.
- **Auto-update channel** — minisign-signed updater artifacts and
  the `latest.json` manifest this very release publishes.

## What doesn't (the honest list)

- **No SFTP server** — file management over SFTP is deferred;
  files move through the API and the install/backup pipelines.
- **No addon marketplace** — addons install by uploading a signed
  `.zimpkg`; there is no central catalog.
- **No telemetry** — the module is specified but ingestion is
  deferred; the engine phones nobody home.
- **No managed-database UI** — the engine stores and operates the
  database rows; a management surface is future work.
- **No legacy client/application API compatibility** —
  Pterodactyl clients that speak the old client API will not work
  against ZIM.
- **Single node** — the engine manages the local node; multi-node
  fleet topology is architecture-ready but not yet implemented.
- **The desktop shell is Linux/Windows/macOS-arm64 only** this
  release (Linux arm64 and macOS x64 builds follow).

## Known rough edges

- **The Linux .deb's package name is `zim`** — the same name as
  Ubuntu's (and Fedora's) unrelated `zim` desktop-wiki package.
  Installing the .deb directly (`sudo apt install ./ZIM_…deb`) is
  fine, but a later plain `apt upgrade` would replace ZIM with the
  wiki app (the archive's version compares higher than 0.1.0).
  **Prefer the AppImage on Debian-family systems** — it is also
  the auto-update payload. The package rename is tracked as the
  release follow-up.
- A stopped instance reports `offline` in the API and UI — there
  is deliberately no separate "stopped" status (the four-state
  Wings vocabulary); a stopped *desired* state is visible per
  instance.
- Container stop on workloads whose PID 1 ignores SIGTERM (plain
  `sh` loops) always burns the 30-second graceful deadline before
  the kill — correct Wings-parity behavior, visible as a slow stop
  for toy containers.
- The engine requires a Docker daemon for lifecycle operations; on
  hosts without one it boots in a degraded API-only mode and
  refuses lifecycle calls with structured 503s rather than failing
  opaquely.

## Install

Grab the artifact for your platform from this release:

- **Windows x64** — `*-setup.exe` (NSIS installer)
- **Linux x64** — `*.deb`, `*.rpm`, or `*.AppImage`
- **macOS arm64** — `*.dmg` (or `*.app.tar.gz`)

Linux prerequisites: the Tauri 2 webview stack (WebKitGTK 4.1) and
Docker for lifecycle operations. The shell lives in your tray,
attaches to the engine in your per-OS app-data directory, and
never stops the engine when it exits.

## Reporting what breaks

Open an issue on this repository with the engine's stderr tail
(the shell logs it to the data directory) and what you did. Alpha
feedback is how the beta gets honest.
