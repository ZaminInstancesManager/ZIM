# Chromium Upstream Manifest

This document records the pinned Chromium revision for ZIM, the import
strategy, the verification status of that strategy in the current workspace,
and the path to a full buildable Chromium fork when a suitable build
environment is provisioned.

It is the canonical reference for the Chromium upstream pin. Other documents
(UPSTREAM_AUDIT, CHROMIUM_ARCHITECTURE, UPSTREAM_MAINTENANCE) reference it
rather than redefining the pin.

## Pinned revision

| Field             | Value                                                        |
|-------------------|--------------------------------------------------------------|
| Project           | Chromium (browser foundation for ZIM)                        |
| Canonical upstream | https://chromium.googlesource.com/chromium/src.git          |
| GitHub mirror     | https://github.com/chromium/chromium                         |
| Pinned tag        | `157.0.8103.0`                                               |
| Pinned commit SHA | `b0a05135bf5a60bb3ab5a1e86a0e55193cb126d0`                  |
| Retrieval date    | 2026-10-13 (UTC+08:00)                                       |
| Retrieval method  | HTTPS via chromium/chromium GitHub mirror (raw + API)       |
| License           | BSD-style (see upstream `LICENSE` file)                      |
| Local patches     | none (patch queue under `chromium/PATCHES/` is empty)        |

The pin is to a specific tag (an immutable, named revision), not a moving
branch. The choice of `157.0.8103.0` was made by enumerating the most recent
tags on the chromium/chromium GitHub mirror via the GitHub API and selecting
the highest published tag at the time of import. The full 40-character commit
SHA above was resolved from the tag ref via
`/repos/chromium/chromium/git/refs/tags/157.0.8103.0`.

## Import strategy

**Current status: DEFERRED subtree import.**

A real, buildable Chromium fork requires substantially more than a `git clone`
of `chromium/src.git`. The full Chromium source tree plus its `DEPS`
dependencies — fetched via `depot_tools` `gclient sync` — is on the order of
100 GB. The ZIM workspace in which this repository was established has the
following practical constraints:

- Total disk available at establishment time: **9.9 GB**, of which 9.3 GB
  was free at the start of work and ~9.2 GB remained free after the Pterodactyl
  Panel and Wings subtree imports completed.
- No `depot_tools` is installed in the workspace; `gclient`, `fetch`, and
  `cl` are not on PATH.
- No Rust toolchain is installed in the workspace (`rustc`, `cargo` not
  present), which would be required for Chromium's Rust-language components.
- Available memory at establishment time: 4.1 GB total, ~3.6 GB free. A
  Chromium link-time build can require tens of gigabytes of memory and is not
  feasible in this environment.

Under these constraints, attempting a full Chromium checkout would either fail
or, worse, silently produce an incomplete tree that might be presented as a
"fork" without being one. The prompt explicitly forbids this: *"If a required
upstream checkout cannot be completed because of storage, network, toolchain
or permission constraints, document the precise limitation. Do not silently
substitute an incomplete copy and call it a fork."*

The strategy is therefore:

1. **Now (architecture phase):** Pin the revision; spot-inspect the upstream
   source at the pinned revision via the chromium/chromium GitHub mirror
   (HTTPS raw fetches and the GitHub contents API). Record findings in
   `docs/audits/CHROMIUM_ARCHITECTURE.md`. The Chromium source tree is **not**
   committed to the ZIM repository at this phase.

2. **First implementation milestone (build environment with sufficient
   capacity):** Perform a real `git subtree add --prefix=chromium/src
   https://chromium.googlesource.com/chromium/src.git 157.0.8103.0` (or, if
   history size becomes prohibitive, `--squash` with an explicit upstream
   commit recorded in this manifest). Then run `gclient sync` against the
   pinned revision to populate `DEPS`. Then verify with `gn gen out/Default`
   and a `ninja -C out/Default chrome` build.

3. **Ongoing maintenance:** Carry ZIM-specific patches under
   `chromium/PATCHES/` with one patch file per logical change, each named
   `NNNN-short-description.patch` and applied via a documented series.
   Re-pin to newer stable Chromium revisions on a deliberate cadence (see
   `docs/architecture/UPSTREAM_MAINTENANCE.md`).

## Verification status

| Verification step                                | Status              | Notes |
|--------------------------------------------------|---------------------|-------|
| Tag exists on chromium/chromium GitHub mirror    | Verified           | API: `/repos/chromium/chromium/git/refs/tags/157.0.8103.0` returned SHA `b0a05135bf5a60bb3ab5a1e86a0e55193cb126d0`. |
| Pinned revision reachable via raw HTTPS         | Verified            | Spot-fetched real source files from `raw.githubusercontent.com/chromium/chromium/157.0.8103.0/...`. |
| Top-level source tree enumerated                 | Verified            | `contents?ref=157.0.8103.0` returned all expected top-level dirs (`chrome/`, `content/`, `components/`, `extensions/`, `base/`, `net/`, `mojo/`, `services/`, `third_party/`, etc.). |
| Key subsystem paths exist at pinned revision     | Verified            | `chrome/browser/ui/tabs/`, `chrome/browser/sessions/`, `chrome/browser/extensions/`, `content/public/browser/` all enumerated. |
| Local Chromium source tree in ZIM repo           | Not present          | Deferred per import strategy above. |
| Local Chromium source tree on disk (workspace)   | Not present          | Deferred per disk constraints above. |
| `DEPS` file present at pinned revision           | Verified            | 4978 lines, fetched via raw HTTPS. |
| `.gn` build entry present at pinned revision     | Verified            | 479 lines, fetched via raw HTTPS. |
| `gclient sync` performed                         | Not performed        | No `depot_tools` in workspace. |
| `gn gen` build configure performed               | Not performed        | No GN toolchain in workspace. |
| `ninja` build performed                          | Not performed        | No Ninja in workspace; insufficient disk for build outputs. |
| ZIM-specific local patches applied               | None yet            | `chromium/PATCHES/` is empty. |

## Local patch queue

`chromium/PATCHES/` is the canonical location for ZIM-specific Chromium patches
once the source tree is imported. Patches will be:

- One file per logical change, named `NNNN-short-description.patch`.
- Applied in numerical order via a documented series file
  `chromium/PATCHES/series`.
- Each patch will have a corresponding entry in
  `docs/architecture/UPSTREAM_MAINTENANCE.md` describing the patch's purpose,
  the source evidence justifying it, the test that protects it, and the
  upstream revision it was first applied against.

No patches exist yet because the source tree is not yet imported. This is
expected at this phase.

## Maintenance strategy

When a Chromium re-pin is desired:

1. Select a new stable tag (prefer a stable-channel release over the latest
   canary/dev tag, unless an explicit feature in a newer revision is required
   and the cost of carrying a non-stable pin is justified).
2. Update this manifest's "Pinned revision" section with the new tag, SHA,
   and retrieval date.
3. If the source tree is already imported, run `git subtree pull
   --prefix=chromium/src <upstream> <new-tag>` and resolve any conflicts
   against the local patch queue.
4. Re-apply the local patch series via `chromium/PATCHES/series`.
5. Re-run the build verification steps listed above.
6. Update the verification status table to reflect what was actually
   performed in the new environment.

## License and third-party notices

Chromium's source is governed by a BSD-style license; the upstream `LICENSE`
file at the pinned revision is preserved at the Chromium root when the source
tree is imported. Chromium also vendors a substantial body of third-party
code under `third_party/`, each component carrying its own `README.chromium`
and license; those notices must be preserved verbatim and surfaced in ZIM's
packaging and distribution material.

ZIM's own modifications to Chromium source (when present) remain governed by
ZIM's Apache 2.0 license for ZIM-original code, with the upstream BSD-style
notice preserved on the unmodified portions. There is no conflict between
these licenses.
