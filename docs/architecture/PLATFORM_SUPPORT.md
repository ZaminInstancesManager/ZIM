# Platform Support (Phase 6)

> **CORRECTION (2026-10-15, AD-20):** The "Browser (Chromium)"
> column is superseded: the frontend is a Tauri 2 desktop shell over
> the system WebView (WebView2 / WKWebView / WebKitGTK) — no
> Chromium build, no patch queue, no depot_tools / 100 GB toolchain.
> Webview availability follows the OS's own webview runtime; engine
> and container-runtime columns are unchanged.

This document specifies ZIM's platform support: which platforms are
first-class, what works on each, what doesn't, and the rationale.

It is canonical for ZIM's platform matrix. The implementation
roadmap (`docs/architecture/IMPLEMENTATION_ROADMAP.md`) records
when each platform reaches first-class status.

## 1. Platform matrix

| Platform               | Browser (Chromium)             | Engine                       | Container runtime (Docker)         | Hosting role (running managed servers) | Status in first ZIM release |
|-------------------------|--------------------------------|------------------------------|--------------------------------------|------------------------------------------|------------------------------|
| Windows 10/11 x64       | Supported (Chromium upstream)  | Supported (Rust cross-compile)| Docker Desktop required              | Hosting role: containerized servers via Docker Desktop | First-class |
| Linux x64 (glibc 2.31+) | Supported                     | Supported                    | Docker Engine required              | Hosting role: containerized servers via Docker Engine; remote-node role | First-class |
| Linux arm64             | Supported (Chromium arm64)     | Supported                    | Docker Engine required              | Hosting role: containerized servers via Docker Engine; remote-node role | First-class (deferred by 1 milestone) |
| macOS 13+ (Apple Silicon) | Supported                     | Supported                    | Docker Desktop / OrbStack required  | Hosting role: containerized servers via Docker | First-class |
| macOS 13+ (Intel)        | Supported                     | Supported                    | Docker Desktop required              | Hosting role: containerized servers via Docker | First-class (deferred by 1 milestone) |
| Remote Linux node (any distro with Docker) | (not applicable; the engine talks to the node) | The remote node runs the node-runtime binary | Docker Engine on the node | Hosting role: containerized servers on the remote node | First-class |
| Remote Windows node      | (not applicable)                | The remote node runs the node-runtime binary | Docker Desktop on the node | Hosting role: containerized servers on the remote node | Deferred |
| FreeBSD / other Unix    | (not supported by Chromium upstream) | (not supported)             | (n/a)                                | (n/a)                                    | Not supported |

## 2. Component responsibilities per platform

### Windows

- **Browser:** Chromium's upstream Windows build, with ZIM's
  local patches (under `chromium/PATCHES/`).
- **Engine:** Rust cross-compiled for `x86_64-pc-windows-msvc`.
  Uses Windows Service integration for engine lifecycle (auto-start
  on user login; restart on crash).
- **Container runtime:** Docker Desktop (WSL2 backend). The user
  must install Docker Desktop; ZIM does not bundle it.
- **Engine data storage:** `%APPDATA%\ZaminMC\ZIM\` (engine DB,
  secrets, instance metadata). Instance data directories default
  to `%USERPROFILE%\Documents\ZIM\instances\{id}\data\` (or a
  user-configured path).
- **URL protocol registration:** `zim://` is registered with
  Windows on install (via the standard `HKEY_CLASSES_ROOT\zim`
  registry entry).
- **Auto-update:** ZIM ships a Windows-specific updater that
  replaces the engine binary and the browser binary (where
  applicable); user is prompted to confirm.

### Linux

- **Browser:** Chromium's upstream Linux build, with ZIM's local
  patches.
- **Engine:** Rust cross-compiled for `x86_64-unknown-linux-gnu`
  and `aarch64-unknown-linux-gnu`. Uses systemd user units for
  engine lifecycle (auto-start on user login; restart on crash).
- **Container runtime:** Docker Engine (or rootless Docker, or
  Podman with Docker-compat layer — preliminary support).
- **Engine data storage:** `~/.local/share/zim/` (XDG).
- **URL protocol registration:** `zim://` is registered via
  `.desktop` file (the standard Linux URL-protocol mechanism);
  placed in `~/.local/share/applications/`.
- **Auto-update:** ZIM ships a Linux updater that supports
  apt/yum repository distribution (preliminary; details in the
  implementation phase).

### macOS

- **Browser:** Chromium's upstream macOS build (universal
  binary), with ZIM's local patches.
- **Engine:** Rust cross-compiled for
  `x86_64-apple-darwin` and `aarch64-apple-darwin` (universal
  binary). Uses `launchd` for engine lifecycle (auto-start on
  user login; restart on crash).
- **Container runtime:** Docker Desktop or OrbStack. The user
  must install one; ZIM does not bundle it.
- **Engine data storage:**
  `~/Library/Application Support/ZaminMC/ZIM/`.
- **URL protocol registration:** `zim://` is registered via the
  standard `Info.plist` `CFBundleURLTypes` mechanism.
- **Auto-update:** ZIM ships a macOS updater that supports
  Sparkle-style updates (signed app bundle; user-prompted).

### Remote Linux node

- **Browser:** (not applicable; the remote node runs no browser).
- **Engine:** The remote node runs the `zim-node-runtime` binary
  (a stripped-down Rust binary that implements the node-runtime
  contract; see `docs/architecture/RUST_ENGINE.md` §9). It does
  not run the full engine.
- **Container runtime:** Docker Engine (rootless recommended).
- **Engine data storage:** `~/.local/share/zim-node/`.
- **URL protocol registration:** (not applicable).
- **Auto-update:** The remote node's binary is updated by the
  central engine pushing a new version (signed); the node-runtime
  contract has version negotiation.

## 3. Isolation capabilities per platform

The prompt is explicit: *"Do not claim equivalent container support
across operating systems without evidence."* ZIM's isolation matrix:

| Platform              | Container runtime         | Isolation quality                          | Notes |
|-----------------------|----------------------------|--------------------------------------------|-------|
| Linux x64/arm64        | Docker Engine              | Full namespace + cgroup isolation          | First-class. |
| Linux x64/arm64        | Podman (Docker-compat)     | Same as Docker (rootless by default)        | Preliminary; not in first release. |
| Windows 10/11 (WSL2)   | Docker Desktop             | Linux VM (WSL2) with namespace isolation   | The container is a Linux container running in a WSL2 VM; Windows-native containers are NOT supported by ZIM. |
| macOS (Apple Silicon)  | Docker Desktop / OrbStack  | Linux VM with namespace isolation           | The container is a Linux container running in a VM. |
| macOS (Intel)          | Docker Desktop             | Linux VM with namespace isolation           | Same. |
| Remote Linux node      | Docker Engine              | Full namespace + cgroup isolation          | First-class. |
| Remote Windows node    | Docker Desktop on the node | Linux VM (WSL2) with namespace isolation   | Deferred. |

### Host runtime (no container)

ZIM may optionally support a "host" runtime where the server
process runs directly on the host (no container). This is
opt-in per instance, requires explicit admin consent, and is
documented as a reduced-isolation mode. It is supported on:

| Platform      | Host runtime supported?                | Notes |
|---------------|------------------------------------------|-------|
| Linux x64/arm64 | Yes (opt-in, admin consent per instance) | Process runs as the engine user; filesystem scoped to the instance's data directory. |
| Windows       | No (deferred; host process on Windows is operationally awkward for game-server isolation) | Use container runtime only. |
| macOS         | No (deferred; same reasoning as Windows) | Use container runtime only. |
| Remote Linux node | Yes (same conditions as local Linux)   | (same as Linux x64) |

## 4. Engine lifecycle per platform

The engine is a long-running process independent of the browser.
Its lifecycle is managed by the OS:

- **Windows:** Windows Service. Auto-start on user login
  (configurable; can be system-wide with admin install). Restart
  on crash with exponential backoff.
- **Linux:** systemd user unit (`~/.config/systemd/user/zim-engine.service`).
  Auto-start on user login. Restart on crash.
- **macOS:** `launchd` agent
  (`~/Library/LaunchAgents/org.zaminmc.zim-engine.plist`).
  Auto-start on user login. Restart on crash.
- **Remote Linux node:** systemd unit on the node.

The engine lifecycle is independent of the browser lifecycle. The
user can close all browser windows; the engine continues to run.
The user can log out (engine continues if system-installed;
engine stops if user-installed and the user's session ends).

## 5. Browser profile and engine data storage

The browser's profile (per Chromium profile) and the engine's data
directory are separate storage locations:

- Browser profile: Chromium's standard profile storage (per
  platform: `%LOCALAPPDATA%\ZaminMC\ZIM\Browser\` on Windows;
  `~/.config/zim-browser/` on Linux;
  `~/Library/Application Support/ZaminMC/ZIM/Browser/` on macOS).
- Engine data: as documented in §2 per platform.
- Instance data: as documented in §2 per platform.

This separation enforces the prompt's rule that browser state and
engine state are independent.

## 6. Logging and crash recovery

- Engine logs: per platform, in the engine data directory's
  `logs/` subdirectory. Rotated daily; retention configurable
  (default 30 days).
- Browser logs: Chromium's standard logging (per platform).
- Crash dumps: Chromium's Crashpad for browser crashes; the
  engine uses `sentry` (or equivalent) for Rust panics (with
  configurable local-only mode that does not phone home).
- Crash recovery: on engine restart after crash, the engine
  reconciles persisted state with runtime observed state for
  every instance (per the lifecycle invariants). On browser
  restart after crash, the browser offers session restore.

## 7. Permissions

- **Windows:** the user must be in the `docker-users` group (Docker
  Desktop's group). The engine does not require admin.
- **Linux:** the user must be in the `docker` group (or use
  rootless Docker / Podman). The engine does not require root.
- **macOS:** the user must have Docker Desktop / OrbStack installed
  and running. The engine does not require admin.
- **Remote Linux node:** the node-runtime user must be in the
  `docker` group (or use rootless Docker).

## 8. Platform-specific isolation capabilities (rationale)

The prompt requires that we not claim equivalent container support
across operating systems without evidence. The rationale for the
asymmetry:

- **Linux:** Docker runs containers natively using Linux namespace
  + cgroup isolation. This is the gold standard. ZIM's
  `DockerRuntime` uses this directly.
- **Windows:** Docker Desktop runs containers in a Linux VM (via
  WSL2). The isolation is equivalent to Linux namespace isolation
  inside the VM, but the VM adds a layer. The container is a Linux
  container; Windows-native containers are not used by ZIM.
- **macOS:** Docker Desktop / OrbStack runs containers in a Linux
  VM (Hypervisor.framework). Same as Windows: Linux containers in
  a VM.
- **Host runtime (Linux only):** ZIM may optionally run the server
  process directly on the host (no container). The isolation is
  *reduced* — the process runs as the engine user with the
  host's filesystem access (scoped to the instance's data directory
  by the engine). This is documented as opt-in and requires admin
  consent per instance.

This is the honest platform support posture: Linux is first-class
for hosting; Windows and macOS are first-class for *running ZIM*
(the browser + engine) but the actual managed servers run in
Linux containers (via Docker Desktop's VM). ZIM does not pretend
that Windows-native containers or macOS-native containers provide
equivalent isolation; the user who wants maximum isolation runs
remote Linux nodes.

## 9. Updates and rollback

- **Engine updates:** per platform, the engine binary is updated
  by the platform's updater (Windows Service update; Linux apt/yum;
  macOS Sparkle). The updater refuses unsigned binaries. Rollback
  is supported by installing a previous version (the user runs the
  previous installer).
- **Browser (Chromium fork) updates:** the ZIM-branded browser is
  updated as a single binary by the same platform updaters. The
  engine and browser can be updated independently (the API version
  contract ensures compatibility across versions; see
  `docs/api/ZIM_API_SPEC.md` §2).
- **Frontend package updates:** handled by the engine's
  frontend-loader (see `docs/architecture/RUST_ENGINE.md` §10).
  Rollback is supported by setting the active package to a
  previous version (which the engine has stored).
- **Egg updates:** handled by `POST /api/v1/eggs/{id}` with a new
  version. The engine stores previous versions; rollback is
  supported by re-importing a previous version.
- **Extension updates:** handled by uploading a new version of
  an extension. The engine stores previous versions; rollback is
  supported by setting the active version.

## 10. Open platform questions

1. **Whether ZIM should support running on Linux distributions
   with musl libc.** Preliminary: yes for the engine (statically
   linked against musl); the browser follows Chromium's upstream
   glibc requirement.
2. **Whether ZIM should support running on Linux without systemd
   (e.g. Alpine Linux with OpenRC).** Preliminary: yes; the engine
   ships a generic "launcher" mode that does not depend on
   systemd, with reduced auto-start reliability (the user must
   configure their init system).
3. **Whether ZIM should support running on BSD-derived systems.**
   Preliminary: no — Chromium upstream does not support BSD;
   ZIM does not duplicate Chromium's platform matrix.
4. **Whether ZIM should support running on Windows on ARM.**
   Preliminary: deferred; Chromium's arm64 Windows build is
   newer and less tested; ZIM tracks it.
5. **Whether ZIM should support running on iOS / Android.**
   Preliminary: no — ZIM is a desktop product.
