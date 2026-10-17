# ZIM Alpha Release Notes — v0.1.0-alpha.2

> **This is an alpha.** ZIM v0.1.0-alpha.2 is the second usable
> release of the Zamin Instances Manager: the first alpha with the
> full platform matrix, the first with a functioning first boot on
> every platform (a defect the packaged-app smoke caught in
> alpha.1's bytes — see below), and the release that closes the
> honest-boundary list the first alpha published. Still explicitly
> **not production-ready**. Expect rough edges; report what breaks.

## What changed since v0.1.0-alpha.1

### The first boot that didn't happen (fixed)

Alpha.1's installers were **dead on arrival on a clean machine**:
the shell tried to spawn the engine into a data directory that did
not exist yet, the spawn failed with a file-not-found before the
engine ever ran, and the only surfacing was the recovery page. The
packaged-app smoke — the exact scenario that exists to catch this —
had never reached that step (it was blocked behind a package-name
collision, fixed hours earlier in the same day). The smoke's new
failure diagnostics made the diagnosis one read: the app-data tree
had WebKit caches but no `engine/` directory at all. The shell now
creates the engine's data directory before spawning into it, the
failure path writes the reason to stderr where operators (and the
smoke) can see it, and a regression test drives the real attach
against a real engine binary on a virgin data directory.

### The package rename (the archive collision, closed)

Alpha.1's Linux package was named `zim` — the same name as Ubuntu's
and Fedora's unrelated `zim` desktop-wiki package, at a lower
version — so a later plain `apt upgrade` would have replaced ZIM
with the wiki app. The package is now **`zamin-zim`** (the product
name is "Zamin ZIM" on the package surfaces; the binaries are
unchanged: `zim_shell`, `zim_engine`). **If you installed the
alpha.1 .deb, remove it before upgrading** (`sudo apt remove zim`)
— the renamed package does not replace it in place. The AppImage
was and remains the auto-update payload on Debian-family systems
and is unaffected. The release smoke proves the rename on every
build: it installs the deb and asserts the archive's `zim` was
never touched.

### The full platform matrix

Five first-class desktop targets, every one built natively on its
own runner and every Linux flavor smoke-tested on its own
architecture:

- **Windows x64** — `*-setup.exe`
- **Linux x64** — `.deb`, `.rpm`, `.AppImage`
- **Linux arm64** — `.deb`, `.rpm`, `.AppImage` *(new)*
- **macOS x64 (Intel)** — `.dmg` *(new)*
- **macOS arm64** — `.dmg`

The update channel's `latest.json` now carries all five platforms'
signed payloads.

### Updates that work on Windows (the coordinated engine shutdown)

The Windows updater no longer fails cleanly when the sidecar engine
is running: the update now downloads first (a failed download never
stops anybody's engine), then asks the engine to drain itself
through the new **engine control plane** — a per-boot token in the
engine's data directory (`engine.token`, readable only by the
machine-local operator context) gating a single
`POST /api/v1/engine/control` action — and only then runs the
installer. Servers keep running through the drain (the engine's
own shutdown design leaves containers running; the new engine
re-adopts them at its next start). The tray gained **"Stop engine —
servers keep running"** over the same mechanism, and the recovery
page's Retry re-spawns it.

### The About surface (`zim://about`)

The alpha notes promised "both versions side by side" — here it is:
the app version, the **running** engine's version (from `/health`),
and the **installed** engine's version (the sidecar binary's own
`--version`), with the honest version-lag note exactly when the two
engines disagree (the engine keeps running through app updates;
the new bytes take over at its next restart). The update channel's
check rides the same page.

### Per-addon attribution in the audit log (AD-29's hardening)

Every API call from an addon page now carries the addon's id (the
vendored SDK stamps it), and the engine's audit records name the
addon **when — and only when — the id belongs to an installed and
enabled addon**. A spoofed id is dropped, never recorded as fact.
Authorization is unchanged: the user whose credentials authorized
the call stays the actor; the addon id is attribution, riding
`metadata.addon_id`, queryable in the audit trail.

### The failure-scenario sweep (nightly)

The scenarios that deserve a slower cadence than every-push CI now
run nightly, and a red sweep files an issue instead of scrolling
away: the E7-E9 visual state mappings (a real crash's recovery
reads yellow, a real crash-loop reads red-with-glyph in the real
browser, unknown reads gray over the real served component), the
interrupted backup (SIGKILL mid-archive → honest orphan
reconciliation → cleanup → the pipeline still works), disk-full
(structured failures, never a hang, engine alive), and the
live-MySQL panel migration (the importer's DSN path, compared
against the fixture path over the same data).

### Health answers `version`

`GET /api/v1/health` now reports the running engine's build version
(the release notes claimed it; the field was missing).

## What works (unchanged from alpha.1)

Everything the first alpha shipped, still exercised end-to-end on
every push: install & first boot, the application chrome (tabs,
omnibox, split view, themes, NTP), the full instance lifecycle with
crash-loop protection and engine-restart reconciliation, the live
event stream, backups, schedules, eggs & installs, five workload
kinds, extensions & themes, Pterodactyl migration, and the
minisign-signed auto-update channel.

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

## Known rough edges

- A stopped instance reports `offline` in the API and UI — there is
  deliberately no separate "stopped" status (the four-state Wings
  vocabulary); a stopped *desired* state is visible per instance.
- Container stop on workloads whose PID 1 ignores SIGTERM (plain
  `sh` loops) always burns the 30-second graceful deadline before
  the kill — correct Wings-parity behavior, visible as a slow stop
  for toy containers.
- The engine requires a Docker daemon for lifecycle operations; on
  hosts without one it boots in a degraded API-only mode and
  refuses lifecycle calls with structured 503s rather than failing
  opaquely.
- macOS x64 and Linux arm64 are new legs: the smoke covers the
  Linux flavors per-architecture, but the macOS Intel leg ships
  build-verified only this release (no packaged-app smoke runs on
  macOS yet — recorded, not hidden).

## Install

Grab the artifact for your platform from this release:

- **Windows x64** — `*-setup.exe` (NSIS installer)
- **Linux x64 / arm64** — `*.deb`, `*.rpm`, or `*.AppImage`
- **macOS x64 / arm64** — `*.dmg` (or `*.app.tar.gz`)

**Upgrading from alpha.1's .deb:** remove the old package first
(`sudo apt remove zim`) — the rename to `zamin-zim` does not
replace it in place. AppImage and Windows installs upgrade through
the channel's Check-for-updates.

Linux prerequisites: the Tauri 2 webview stack (WebKitGTK 4.1) and
Docker for lifecycle operations. The shell lives in your tray,
attaches to the engine in your per-OS app-data directory, and never
stops the engine when it exits.

## Reporting what breaks

Open an issue on this repository with the engine's stderr tail
(the shell logs it to the data directory) and what you did. Alpha
feedback is how the beta gets honest.
