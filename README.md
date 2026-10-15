# ZIM — Zamin Instances Manager

ZIM is an independent desktop product of the ZaminMC ecosystem: a
Chrome-inspired desktop application powered by a Rust-based
workload-management engine. ZIM lets the user manage long-running
workloads — game servers, databases, websites, development processes,
generic containers, and future workload types introduced by
first-party modules or third-party extensions — through a
Chrome-style tabbed interface, while keeping the application and the
management engine as separate, independently-recoverable
authorities.

Minecraft and Pterodactyl are the original workload and reference
implementation that ZIM was bootstrapped from; the engine is generalized
beyond them through a typed workload abstraction with capability traits
(see `docs/architecture/WORKLOAD_ABSTRACTION.md`).

This repository is the engineering foundation for ZIM. It contains the real
upstream source trees of Pterodactyl Panel and Pterodactyl Wings (imported via
subtree merge with full upstream history preserved), the cancelled Chromium
import manifest (kept as provenance; see AD-20), the source-backed audits of
the Pterodactyl codebases and of Chrome's UX platform, the architecture and
API contracts for the Rust engine, the desktop frontend, and workload
abstraction, and the roadmap that subsequent implementation work will follow.

## What this repository is and is not

**It is:**

- A real, source-backed project: Pterodactyl Panel and Wings are imported
  as audited reference trees; Chrome's UX is the design reference for the
  frontend (no Chromium source — see AD-20).
- A working ZIM monorepo: Pterodactyl Panel and Wings source trees are present
  as `upstream/pterodactyl-panel/` and `upstream/pterodactyl-wings/` with their
  complete upstream history reachable through `git log`.
- The authoritative engineering specification for ZIM's Rust engine, browser
  integration, frontend replacement model, extension SDK, security boundaries
  and lifecycle invariants.
- A reproducible upstream pin: every imported project has a recorded revision,
  retrieval method, license and verification status in `docs/audits/`.

**It is not:**

- Dependent on Chromium source. The Chromium import is CANCELLED
  (AD-20, 2026-10-15): ZIM is a desktop application with a
  Chrome-inspired UI, not a browser. The desktop shell renders via the
  OS webview (WebView2 / WKWebView / WebKitGTK) — no Chromium source,
  no patch queue, no 100 GB build, ever. `chromium/UPSTREAM_MANIFEST.md`
  is kept as provenance for the cancelled decision; the pinned-revision
  audit (`docs/audits/CHROMIUM_ARCHITECTURE.md`) is reframed as a
  Chrome platform/UX reference for the frontend.
- A working ZIM runtime — partially. The Rust engine workspace
  (`zim/engine/`) is under active implementation following
  `docs/architecture/IMPLEMENTATION_ROADMAP.md`: the domain model +
  SQLite persistence (M3), authentication + authorization (M4), the
  HTTPS loopback API surface with the runnable `zim_engine` binary
  (M5), the runtime abstraction + Docker runtime (M6 — instance
  lifecycle with power-queue serialization, crash-loop protection,
  engine-restart reconciliation, and Wings-parity container
  management over `pterodactyl0`), streaming (M7 — the
  `zim_engine_streaming` fan-out hub with per-instance event
  generations, replay-from-checkpoint, drop-oldest backpressure,
  the `/api/v1/stream` WebSocket endpoint with per-tab capability
  token auth, supervision-published `instance.state_change` and
  `instance.resource_usage` events, and the console/metrics read
  endpoints), the full durable-jobs milestone (M8 — the
  `zim_engine_jobs` executor, the `zim_engine_backups`
  create/restore pipeline, the `zim_engine_transfers`
  two-phase-commit node move, and the `zim_engine_installs`
  constrained-container install pipeline over the `InstallRunner`
  port), the egg importer and per-instance cron scheduler (M9 —
  `zim_engine_eggs` and `zim_engine_scheduler`), the Pterodactyl
  Panel migration importer (M10 —
  `zim_engine_migrate_pterodactyl`), the frontend loader +
  default frontend (M11 — `zim_engine_frontend_loader` and the
  vanilla-TS/Web-Components package under `zim/frontend/`), the
  workload integration SDK + the five first-party integration
  crates (M16), the Minecraft integration at egg parity (M17 —
  the real file API, the egg-derived GameServerSpec, the console
  convention, the fakemc parity suite), the Database integration
  (M18 — Postgres / MariaDB / MySQL / Redis: the engine table,
  the readiness probes, the quiesced snapshot/restore family,
  the multi-step migration plan, and the real-container parity
  suite), and the remaining three first-party integrations (M19
  — the Website kind's static nginx carrier with the
  release-directory deploy family and the HTTP/TCP health
  checks, the DevProcess kind's `notify`-backed file-watch
  restart supervision, and the GenericContainer kind's
  placement-time capability negotiation). Remaining: the Chromium
  import (M12) and the integration milestones (M13-M15, gated on
  it). Operations that need those milestones answer `501
  not_implemented` per the API spec's transitional vocabulary.
- A re-skinned Pterodactyl. The Panel's PHP/Laravel backend and Wings's Go
  daemon are preserved in `upstream/` as audit references and behavioral
  contracts; they are not part of ZIM's runtime architecture.

## Repository layout

```text
ZIM/
├── chromium/                    # Cancelled Chromium import manifest (provenance; AD-20)
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
| Chromium             | https://chromium.googlesource.com/chromium/src.git   | 157.0.8103.0 | 2026-10-13   | cancelled (AD-20; UX reference only) | BSD-style |

Full traceable commit hashes and verification status are recorded in
`docs/audits/UPSTREAM_AUDIT.md`.

## How to read this repository

Start with:

1. `docs/audits/BASELINE.md` — Phase 0 workspace baseline.
2. `docs/audits/UPSTREAM_AUDIT.md` — what is imported, how, and what is verified.
3. `docs/audits/CHROMIUM_ARCHITECTURE.md` — Chrome platform/UX reference (integration cancelled; see AD-20).
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
