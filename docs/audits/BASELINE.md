# Phase 0 — Workspace Baseline Report

This document records the state of the workspace and the ZaminInstancesManager
organization at the time work began. It is the canonical baseline against
which later changes are compared.

## Local workspace state at establishment

| Check                                | Result                                              |
|--------------------------------------|-----------------------------------------------------|
| Host operating system                | Linux (containerized; rootfs-backed filesystem)     |
| Total disk on root filesystem        | 9.9 GB                                              |
| Free disk at establishment           | 9.3 GB                                              |
| Free disk after Panel + Wings imports | ~9.2 GB                                            |
| Total memory                         | 4.1 GB                                              |
| Free memory at establishment         | ~3.6 GB                                             |
| Swap                                 | none                                                |
| `git`                                | 2.47.3 (installed)                                  |
| `python3`                            | 3.12.15 (installed)                                 |
| `node`                               | v24.21.0 (installed)                                |
| `curl` / `wget`                      | both installed                                      |
| `depot_tools` / `gclient`            | **not installed**                                   |
| `rustc` / `cargo`                    | **not installed**                                   |
| Chromium source checkout on disk     | not present                                          |
| Pterodactyl Panel source on disk    | not present (pre-import)                            |
| Pterodactyl Wings source on disk    | not present (pre-import)                            |
| Network reachability to github.com  | reachable (HTTP 200, ~110 ms RTT)                   |
| Network reachability to chromium.googlesource.com | reachable (HTTP 200)                  |
| PAT validity                         | valid; authenticated as GitHub user `Abasing` (id 140362885) |

The workspace therefore supports git operations on remote repositories of
moderate size (Pterodactyl Panel at ~37 MB and Pterodactyl Wings at ~4 MB
fitted comfortably), supports HTTPS file fetches against the chromium/
chromium GitHub mirror, and supports GitHub REST API calls (with the higher
authenticated rate limit). It does not support a full Chromium source +
dependencies checkout, nor any Rust or Chromium build.

## ZaminInstancesManager organization state at establishment

| Check                                | Result                                              |
|--------------------------------------|-----------------------------------------------------|
| Organization                         | `ZaminInstancesManager`                             |
| Plan                                 | Free                                                |
| Owned private repos                  | 0                                                   |
| Total private repos                  | 0                                                   |
| Public repos                         | 1 (`ZIM`)                                           |
| Authenticated user                   | `Abasing` (admin on the organization)               |
| Existing ZIM repo description         | "ZIM — Zamin Instances Manager: a desktop panel, CLI, and resident daemon for running and managing Minecraft servers." |

The pre-existing ZIM repository description focuses on Minecraft servers
specifically and lists "desktop panel, CLI, and resident daemon" as the
product shape. The comprehensive prompt's product vision is broader: ZIM is a
Chromium + Pterodactyl hard fork with a browser-native interface for managing
game servers in general, where the desktop panel is the browser itself. The
existing description is not contradicted by the broader vision but is narrower
than it. No description change has been made in this assignment; the
description may be revised in a separate, focused change once the product
identity is reaffirmed by the project owner.

## Pre-existing ZIM repository state

The ZIM repository at `https://github.com/ZaminInstancesManager/ZIM.git`
contained, at the start of work:

- One branch: `main`.
- Two commits:
  - `93352213a Initial commit` (created at repository creation).
  - `2fd69a1da Add files via upload` (committed 2026-10-13 15:46:09 UTC by
    Zamin Bhutto, GitHub user Abasing).
- Two files at HEAD:
  - `LICENSE` — Apache License 2.0 (201 lines, 11357 bytes). Apache 2.0 is a
    permissive license compatible with both upstream BSD-style (Chromium) and
    MIT (Pterodactyl) licenses, and is a sensible choice for ZIM.
  - `ZIM-ICON-SOURCE.png` — ZIM brand icon source (1.36 MB). The "SOURCE"
    suffix suggests this is the editable/source form of the icon, presumably
    with derived runtime assets (ICO, ICNS, PNG favicons at various sizes)
    produced from it for packaging.

Both files were preserved untouched by all subsequent work. The Apache 2.0
license and the brand icon are the only pre-existing user-authored content in
the repository and they are treated as immutable for the remainder of this
assignment.

## Decisions and constraints carried forward

The following facts established by this baseline govern all subsequent
phases:

1. **Disk budget.** With ~9.2 GB free after Panel and Wings imports, the
   workspace cannot host a Chromium source tree. The Chromium import strategy
   in `chromium/UPSTREAM_MANIFEST.md` reflects this. Any work that attempts
   to allocate additional large files (full Pterodactyl history expansion,
   binary artifacts, build outputs) must respect this budget.

2. **No Rust toolchain.** The Rust engine specification in
   `docs/architecture/RUST_ENGINE.md` is a contract, not a built artifact.
   The first implementation milestone must include provisioning a Rust
   toolchain; this is recorded as a blocker in
   `docs/architecture/IMPLEMENTATION_ROADMAP.md`.

3. **No depot_tools.** Even if disk were available, a Chromium build would
   require installing depot_tools and running `gclient sync` against the
   pinned revision. Build-readiness verification for Chromium is therefore
   blocked by environment and recorded as such in
   `docs/audits/UPSTREAM_AUDIT.md`.

4. **License posture.** ZIM is Apache 2.0; upstream Pterodactyl is MIT;
   upstream Chromium is BSD-style. All three are permissive and mutually
   compatible. ZIM must preserve upstream license notices verbatim in the
   `upstream/` subtrees and in the Chromium subtree when it is imported.

5. **Identity of commits.** All work in this assignment is committed under
   the GitHub user `Abasing` (the project owner), with git identity
   `Abasing <140362885+Abasing@users.noreply.github.com>`. No other identity
   is used. GPG signing is disabled in the local clone because the workspace
   has no GPG agent and the existing ZIM commits are unsigned.

6. **Scope of authority.** Work is strictly limited to the
   `ZaminInstancesManager` organization. No operations were performed against
   repositories outside the organization; the only external reads were
   read-only fetches from the public `pterodactyl/panel`,
   `pterodactyl/wings`, and `chromium/chromium` repositories and read-only
   metadata API calls against them. No PAT-authenticated writes were made
   outside `ZaminInstancesManager/ZIM`.

## What changed in Phase 0

Phase 0 is inspection-only. No commits were made to the ZIM repository in
Phase 0; the first commit in this assignment is in Phase 1 (upstream import)
and is recorded in `docs/audits/UPSTREAM_AUDIT.md`. The local workspace
directory `/home/z/zim-work` was created as the working clone of the ZIM
repository; `/home/z/chromium-inspect` was created as a scratch directory for
spot-fetched Chromium source files used during the audit (these files are not
committed to the ZIM repository and are listed in
`docs/audits/CHROMIUM_ARCHITECTURE.md` as inspection evidence only).
