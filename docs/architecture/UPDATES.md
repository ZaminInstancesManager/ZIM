# Update Channel (M25)

> Status: **Delivered (M25)** — mechanics live; alpha semantics.
> Owner: the desktop shell (`zim/shell/src/updater.rs`), the release
> pipeline (`.github/workflows/app-release.yml`), this document.

This document is the contract for how a ZIM desktop installation
updates itself. It is normative for the shell's updater and the
release pipeline; the engine has no update surface of its own (it
is updated as part of the app bundle).

## 1. Model: one channel, consent-shaped

There is exactly **one channel**: GitHub Releases of this
repository. Every tagged build publishes:

- the per-platform **installers** (NSIS `.exe` + MSI on Windows,
  `.deb` / `.rpm` / `.AppImage` on Linux, `.dmg` + `.app.tar.gz` on
  macOS),
- the **updater payloads** with their **minisign signatures**
  (`.sig` files, produced by the Tauri bundler from the
  `TAURI_SIGNING_PRIVATE_KEY` Actions secret),
- `latest.json` — the **channel manifest** at the stable URL
  `https://github.com/ZaminInstancesManager/ZIM/releases/latest/download/latest.json`.

The shell's updater reads that manifest. **Updates install only on
explicit user action** — the tray's "Check for updates…" item. There
is no check-on-boot and no background download in the alpha; the
user's click is the consent, and the label on the item they clicked
is where the progress surfaces ("Downloading v0.1.1… 42%",
"Installing…", "Up to date", "Update check failed"). An in-app
update page is release polish, not mechanics, and lands with M26.

## 2. Trust: what the signature actually guarantees

- The **public key** is baked into the shell binary at build time
  (`zim/shell/tauri.conf.json` → `plugins.updater.pubkey`).
- The **private key** lives only in the repository's Actions
  secrets (`TAURI_SIGNING_PRIVATE_KEY`, with its password in
  `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`). It was generated for this
  channel and never leaves Actions; the org owner should keep an
  offline copy — losing it means a new key must be shipped with a
  new installer (the channel's hard recovery path).
- A release install proceeds only if the payload's signature
  verifies against the baked-in key. A compromised release page
  without the key cannot push code to an installed shell.

The signing key is **channel authority, not release quality**: the
alpha channel ships builds that verify and still contain
alpha-grade behavior. The notes field of `latest.json` says so on
every release.

## 3. Granularity: the bundle updates, the engine keeps running

One update payload carries **shell + engine together** (the engine
is the `zim_engine` sidecar installed next to the shell executable
— the M25 bundle layout `resolve_engine_path()` probes). But the
engine is a detached, independently supervised process by design
(the shell is disposable; the engine and the servers are not):

- The updater replaces the shell and relaunches it.
- The **running engine process is not killed** by the update — the
  next attach probe finds it healthy and keeps using it.
- The **new** engine bytes take over the next time the engine
  itself restarts (reboot, crash, or an explicit stop).
- Consequence: for a window after an update, the running engine
  version can lag the installed engine version. `GET /api/v1/health`
  reports the running engine's version; the About surface (M26)
  will surface both versions side by side.

**Known alpha limitation (Windows):** the NSIS installer cannot
replace a running executable. If the detached engine is executing
from the install directory, a Windows update will fail cleanly
(the tray item reports "Update failed") until the engine has been
stopped or the machine rebooted. The POSIX platforms replace the
file under the running process without issue. The durable fix — a
coordinated engine shutdown as part of the update's pre-install
step — is M26 scope and is recorded in the roadmap.

## 4. Rollback

There is no automated downgrade: the updater refuses versions not
greater than the current one (the default comparator). Rollback is
**installer-based**: every release's installers remain attached to
its release page, and installing an older build over a newer one
replaces the bundle wholesale. The updater then simply sees the
older version as current. This is the honest alpha answer; delta
updates and downgrade channels are out of scope until there is a
production channel at all.

## 5. The manifest format

`latest.json` is assembled by the release pipeline
(`zim/shell/scripts/make_latest_json.py`) from the bundler's own
signature files:

```json
{
  "version": "0.1.0",
  "notes": "ZIM alpha 0.1.0. See the release page …",
  "pub_date": "2026-10-16T00:00:00+00:00",
  "platforms": {
    "windows-x86_64": { "signature": "…", "url": "…-setup.exe" },
    "linux-x86_64":   { "signature": "….AppImage.sig content", "url": "….AppImage" },
    "darwin-aarch64": { "signature": "….app.tar.sig content", "url": "….app.tar.gz" }
  }
}
```

Platform keys follow the updater's `{os}-{arch}` scheme as the
bundler emits them. A platform absent from `platforms` is simply
not offered an update on that platform.

## 6. What this channel is not

- Not a production channel: no staged rollouts, no telemetry, no
  mandatory updates, no revocation list.
- Not the addon/theme channel: extensions and themes have their own
  trust model (FRONTEND_EXTENSION_SPEC §3.4/§4.1) over the engine's
  trust store — independent of this document.
- Not the engine's data format: `engine.db` schema migrations are
  the engine's own boot-time discipline (RUST_ENGINE.md §persistence),
  orthogonal to app updates.
