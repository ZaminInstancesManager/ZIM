# ZIM — Zamin Instances Manager

ZIM is an independent desktop product of the ZaminMC ecosystem that combines a
hard-forked Chromium browser with a Rust-based game-server management engine
derived from Pterodactyl's domain. ZIM lets the user manage long-running game
servers through ordinary browser tabs instead of a separate hosting control
panel, while keeping the browser and the server engine as separate,
independently-recoverable authorities.

This repository is the engineering foundation for ZIM. It contains the real
upstream source trees of Pterodactyl Panel and Pterodactyl Wings (imported via
subtree merge with full upstream history preserved), the pinned-revision
manifest and import strategy for Chromium, the source-backed audit of all three
upstream codebases, the architecture and API contracts for the Rust engine and
browser integration, and the roadmap that subsequent implementation work will
follow.

## What this repository is and is not

**It is:**

- A real, source-backed hard fork of Chromium and Pterodactyl.
- A working ZIM monorepo: Pterodactyl Panel and Wings source trees are present
  as `upstream/pterodactyl-panel/` and `upstream/pterodactyl-wings/` with their
  complete upstream history reachable through `git log`.
- The authoritative engineering specification for ZIM's Rust engine, browser
  integration, frontend replacement model, extension SDK, security boundaries
  and lifecycle invariants.
- A reproducible upstream pin: every imported project has a recorded revision,
  retrieval method, license and verification status in `docs/audits/`.

**It is not:**

- A buildable Chromium checkout. A full Chromium source-plus-dependencies
  checkout requires on the order of 100 GB of disk and the `depot_tools` /
  `gclient` toolchain; the workspace in which this repository was established
  has roughly 9 GB of disk available and no `depot_tools`. The Chromium source
  tree is therefore deferred for import until a build environment with
  sufficient capacity is provisioned. The pinned revision, import strategy and
  build-readiness constraints are documented in
  `chromium/UPSTREAM_MANIFEST.md` and `docs/audits/UPSTREAM_AUDIT.md`, and the
  Chromium architecture audit in `docs/audits/CHROMIUM_ARCHITECTURE.md` is
  backed by direct spot-inspection of the upstream source at the pinned
  revision via the chromium/chromium GitHub mirror.
- A working ZIM runtime. No Rust engine code has been written in this
  repository yet. The architecture, contracts and roadmap identify the first
  implementation milestone and its acceptance criteria; that milestone is
  deliberately out of scope for this assignment (see
  `docs/architecture/IMPLEMENTATION_ROADMAP.md` and the "Implementation
  boundary" section of `docs/decisions/DECISIONS.md`).
- A re-skinned Pterodactyl. The Panel's PHP/Laravel backend and Wings's Go
  daemon are preserved in `upstream/` as audit references and behavioral
  contracts; they are not part of ZIM's runtime architecture.

## Repository layout

```text
ZIM/
├── chromium/                     # Chromium import manifest and patch queue
│   ├── UPSTREAM_MANIFEST.md      # Pinned revision, import strategy, status
│   └── PATCHES/                  # Local Chromium patches (initially empty)
├── upstream/
│   ├── pterodactyl-panel/        # Pterodactyl Panel v1.15.1 (subtree-merged)
│   └── pterodactyl-wings/        # Pterodactyl Wings v1.13.3 (subtree-merged)
├── zim/                          # ZIM's own code (skeletons for now)
│   ├── engine/                   # Rust engine workspace (TBD)
│   ├── contracts/                # Versioned IPC/API contracts
│   ├── frontend/                 # Default frontend package
│   ├── extensions/               # Bundled extensions
│   └── integration/              # Browser ↔ engine integration layer
├── docs/
│   ├── audits/                   # Upstream audits, baseline, dispositions
│   ├── architecture/             # ZIM architecture, Rust engine, browser integration
│   ├── api/                      # API + frontend/extension specs
│   ├── compatibility/            # Pterodactyl migration/compat
│   ├── security/                 # Threat model
│   └── decisions/               # ADRs
├── tools/                        # Repo scripts
├── tests/                        # Cross-component tests
├── LICENSE                       # Apache 2.0
└── ZIM-ICON-SOURCE.png           # ZIM brand icon source
```

## Upstream pins

| Project              | Upstream URL                                          | Revision   | Retrieval date | Import method | License |
|----------------------|-------------------------------------------------------|------------|----------------|---------------|---------|
| Pterodactyl Panel    | https://github.com/pterodactyl/panel                  | v1.15.1    | 2026-10-13     | git subtree   | MIT     |
| Pterodactyl Wings    | https://github.com/pterodactyl/wings                  | v1.13.3    | 2026-10-13     | git subtree   | MIT     |
| Chromium             | https://chromium.googlesource.com/chromium/src.git   | 157.0.8103.0 | 2026-10-13   | deferred (see manifest) | BSD-style |

Full traceable commit hashes and verification status are recorded in
`docs/audits/UPSTREAM_AUDIT.md`.

## How to read this repository

Start with:

1. `docs/audits/BASELINE.md` — Phase 0 workspace baseline.
2. `docs/audits/UPSTREAM_AUDIT.md` — what is imported, how, and what is verified.
3. `docs/audits/CHROMIUM_ARCHITECTURE.md` — what Chromium actually provides.
4. `docs/audits/PTERODACTYL_ARCHITECTURE.md` — what Panel and Wings actually do.
5. `docs/audits/COMPONENT_DISPOSITION.md` — what ZIM keeps, adapts, replaces, removes, defers, or investigates.
6. `docs/architecture/ZIM_ARCHITECTURE.md` — the authoritative ZIM architecture.
7. `docs/architecture/IMPLEMENTATION_ROADMAP.md` — what happens next.

## License

ZIM's own code is Apache 2.0 (see `LICENSE`). Upstream source trees in
`upstream/` retain their original licenses (MIT for Pterodactyl, BSD-style for
Chromium when imported); upstream license files are preserved at the root of
each imported subtree.

## Status

This repository is the *engineering foundation* for ZIM, not a shipping
product. The architecture, contracts, audit evidence and roadmap recorded here
are intended to allow a separate implementation phase to proceed without
re-inventing the architecture as it goes. See
`docs/architecture/IMPLEMENTATION_ROADMAP.md` for the first milestone and its
acceptance criteria.
