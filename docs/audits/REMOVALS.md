# Removals — Phase 5 Record

> **CORRECTION (2026-10-15, AD-20):** Chromium-fork references herein
> are superseded — ZIM is a Chrome-inspired desktop application, not a
> browser. Retained as the historical record of the superseded plan;
> see `docs/architecture/DECISIONS.md` AD-20.

This document records every upstream component that has been removed,
disabled, or quarantined in the ZIM repository, with the evidence
supporting each removal and the validation performed.

Phase 5's purpose is to *establish a clean ZIM product*, not to
indiscriminately delete unfamiliar code. The prompt's cleanup rules
are strict:

> *"Cleanup must follow the component-disposition audit. ... Prefer the
> least invasive solution that delivers the intended product behavior.
> ... If safe removal cannot be established, defer it. A documented
> unused component is better than a broken fork."*

## Summary of removals in the architecture phase

**Zero upstream source has been removed in the architecture phase.**

This is not a failure to perform Phase 5; it is the correct outcome of
Phase 5 for this repository at this phase, for the following reasons:

1. **The Chromium source tree is not imported.** There is no Chromium
   source in the ZIM repository to remove. Cleanup decisions for
   Chromium surfaces (e.g. removing or disabling an irrelevant bundled
   application, or disabling a feature flag for an unused product
   surface) are deferred until the Chromium source is imported. Each
   such removal will be a separate, evidence-backed entry in this
   document, with the corresponding disposition in
   `docs/audits/COMPONENT_DISPOSITION.md`.

2. **The Pterodactyl Panel and Wings subtrees are preserved as audit
   reference and behavioral-contract reference.** Removing the Panel's
   PHP services or Wings's Go supervisor during the architecture phase
   would destroy the evidence base for the Rust engine's behavioral
   contracts. The disposition matrix
   (`docs/audits/COMPONENT_DISPOSITION.md`) classifies these as
   **REPLACE** (behavior; source preserved as reference), not as
   **REMOVE**. The source is therefore not deleted; it is preserved
   in `upstream/` and is not part of ZIM's runtime architecture.

3. **The Panel's traditional dashboard frontend is not built by ZIM.**
   The disposition matrix classifies the Panel frontend
   (`resources/`, `public/`) as **REMOVE** *from ZIM's runtime* — but
   the source is preserved in the `upstream/pterodactyl-panel/`
   subtree as audit reference. ZIM's default frontend is authored
   fresh under `zim/frontend/`; no Panel frontend source is deleted
   from the repository.

4. **The Panel's deployment artifacts (Dockerfile, docker-compose,
   Nix flake) are not used by ZIM.** Same disposition: source
   preserved in `upstream/`, not used in ZIM's runtime; not deleted.

## What *is* removed or disabled

The only removal performed in the architecture phase is internal to
the ZIM repository's own structure, not to upstream source:

| Item                                | Action                  | Reason                                                            | Evidence |
|-------------------------------------|-------------------------|-------------------------------------------------------------------|----------|
| `/home/z/chromium-inspect/` directory (workspace-local, NOT in the ZIM repo) | Kept as inspection-only scratch; not committed to the ZIM repo | Spot-fetched Chromium source files used during audit are inspection evidence, not part of ZIM's source tree | Documented in `docs/audits/CHROMIUM_ARCHITECTURE.md` §evidence list |

No other removals have been performed.

## Deferred removals (Phase 5 candidates for the implementation phase)

The following removals are *candidates* for the implementation phase,
when the engine reaches sufficient maturity that the corresponding
upstream source is no longer needed as audit reference. Each
candidate has its evidence and validation requirements documented so
the implementation phase can execute the removal safely.

### Candidate R1: Panel frontend source, after ZIM default frontend is feature-complete

**Component:** `upstream/pterodactyl-panel/resources/`,
`upstream/pterodactyl-panel/public/`.
**Current disposition:** REMOVE (from runtime), source preserved as
reference.
**Removal trigger:** ZIM default frontend achieves parity with the
behavioral contract extracted from the Panel frontend in
`docs/api/FRONTEND_EXTENSION_SPEC.md`. Specifically, every capability
the Panel frontend exposes (server list, console, files, schedules,
backups, subusers, settings, databases, admin) has a ZIM replacement
with passing E2E tests.
**Validation required before removal:**
- Run ZIM's E2E test suite; all tests pass.
- Audit each Panel frontend Vue component against the ZIM
  replacement; document any behavioral gap.
- Confirm no external tool consumes the Panel frontend source as a
  build input.
**Residual risk:** if a future ZIM release regresses a frontend
behavior, the reference is gone. Mitigation: the behavioral contract
in `docs/api/FRONTEND_EXTENSION_SPEC.md` is the canonical
specification and survives the removal.
**Status:** DEFERRED.

### Candidate R2: Panel deployment artifacts, after ZIM packaging is complete

**Component:** `upstream/pterodactyl-panel/Dockerfile`,
`docker-compose.example.yml`, `flake.nix`, `flake.lock`.
**Current disposition:** REMOVE (from runtime), source preserved as
reference.
**Removal trigger:** ZIM's cross-platform packaging
(`docs/architecture/PLATFORM_SUPPORT.md`) is complete for Windows,
Linux, and macOS.
**Validation required before removal:**
- ZIM packages build and run on all three target platforms.
- No external tool consumes the Panel's Dockerfile or Nix flake as a
  build input for ZIM.
**Residual risk:** none beyond the loss of the Panel's deployment
example as reference.
**Status:** DEFERRED.

### Candidate R3: Wings daemon entry and router, after engine reaches parity

**Component:** `upstream/pterodactyl-wings/wings.go`,
`cmd/`, `router/`, `remote/`.
**Current disposition:** REPLACE (behavior), source preserved as
reference.
**Removal trigger:** ZIM engine reaches lifecycle parity with Wings
(start/stop/restart/kill, crash detection, install, transfers,
backups, console streaming, files, schedules) with passing
engine-integration tests.
**Validation required before removal:**
- The engine's integration test suite covers every Wings behavior
  documented in `docs/audits/PTERODACTYL_ARCHITECTURE.md` §7.2.
- Every Wings `*_test.go` behavior has a Rust integration test that
  reproduces it (where the behavior is in scope for ZIM).
- A clean migration path from an existing Wings deployment to a ZIM
  engine deployment is documented in
  `docs/compatibility/MIGRATION_AND_COMPATIBILITY.md` and verified
  by a migration E2E test.
**Residual risk:** loss of Wings source as a contract reference for
edge-case behaviors (e.g. crash-loop backoff thresholds). Mitigation:
the behavioral invariants are recorded in
`docs/architecture/RUST_ENGINE.md` and the security properties in
`docs/security/THREAT_MODEL.md`; both survive the removal.
**Status:** DEFERRED.

### Candidate R4: Panel application code, after engine + migration importer reach parity

**Component:** `upstream/pterodactyl-panel/app/`,
`database/migrations/`, `routes/`, `config/`, `bootstrap/`.
**Current disposition:** REPLACE (behavior), source preserved as
reference.
**Removal trigger:** ZIM engine + ZIM migration importer reach
parity with Panel's behavioral contracts (server creation, deletion,
configuration, permissions, allocations, nodes, schedules, backups,
databases, subusers, activity, API keys).
**Validation required before removal:**
- ZIM engine's integration tests cover every Panel service contract.
- The migration importer reads an existing Pterodactyl MySQL database
  and produces a ZIM database without data loss; verified by an
  E2E migration test against a fixture Pterodactyl database.
- The legacy client API surface (if ZIM preserves it) is implemented
  and its tests pass.
**Residual risk:** loss of Panel source as a contract reference.
Mitigation: behavioral contracts are in
`docs/api/ZIM_API_SPEC.md` and
`docs/compatibility/MIGRATION_AND_COMPATIBILITY.md`; both survive.
**Status:** DEFERRED.

### Candidate R5: Chromium bundled applications and demonstration pages (TBD)

**Component:** (to be enumerated after Chromium source is imported)
potential candidates include `chrome/browser/ui/webui/*` pages not
relevant to ZIM, demonstration features under `chrome/browser/ui/views/`,
etc.
**Current disposition:** INVESTIGATE (cannot be enumerated without
the imported source tree).
**Removal trigger:** after Chromium source is imported, an evidence
pass identifies bundled applications and demo pages whose removal
does not break required features.
**Validation required before any removal:**
- Trace the build dependencies of the candidate component.
- Confirm no required feature depends on it.
- Document the patch (a `BUILD.gn` modification or feature flag
  disable) under `chromium/PATCHES/`.
- Run upstream regression tests; classify any failures.
**Residual risk:** breaking an obscure feature that depends on the
removed component. Mitigation: prefer disabling via feature flag over
deleting source; recoverability is built into the flag.
**Status:** DEFERRED (blocked by Chromium source import).

## What will NOT be removed

Per the prompt's Phase 5 Chromium cleanup section, the following are
**explicitly preserved** regardless of cleanup decisions:

- Web rendering and JavaScript (V8, Blink, Skia, etc.).
- Navigation and networking.
- Tabs and windows.
- Bookmarks and history.
- Session restoration.
- Browser profiles.
- Split view (verified first-class in Chromium 157).
- Browser security (sandbox, site isolation).
- Extensions (MV3 platform).
- Internal ZIM pages (WebUI infrastructure).
- Keyboard shortcuts.
- Browser settings.
- Cross-platform builds.
- Crash handling and diagnostics.
- Upstream tests and development tooling.
- Upstream synchronization infrastructure.

For Pterodactyl, the following behavioral contracts are **explicitly
preserved** (as contracts, not as source):

- API semantics of the legacy client surface (deferred).
- Permission behavior (Permission vocabulary).
- Server definitions (egg format).
- Installation compatibility (egg importer).
- Database and backup formats (migration importer).
- Process supervision behavior (Rust engine reproduces).
- Runtime limitations (documented in
  `docs/architecture/RUST_ENGINE.md`).
- Security controls (preserved where they protect users; tightened
  where they don't — see `docs/security/THREAT_MODEL.md`).
- Failure recovery (preserved as behavioral invariants).
- Existing deployment assumptions (documented in
  `docs/compatibility/MIGRATION_AND_COMPATIBILITY.md`).

## Validation status of Phase 5

| Check                                                  | Status       | Notes |
|--------------------------------------------------------|--------------|-------|
| No unverified mass deletion occurred                  | Verified     | Zero upstream source deleted in this phase. |
| Browser infrastructure required by ZIM remains intact | Verified     | No Chromium source to delete (deferred per manifest); the disposition matrix marks required Chromium components KEEP. |
| Server-management infrastructure required by ZIM remains intact (as audit reference) | Verified | Pterodactyl subtrees preserved in `upstream/`. |
| All cleanup decisions recorded                        | Verified     | This document is the canonical record. |
| Validation results recorded                            | Verified     | This section. |
| No accidental nested git repos                       | Verified     | Pterodactyl imported via subtree merge, not as nested repos. Verified by `git log` showing subtree merge commits. |
| Upstream licensing and notices remain available      | Verified     | `upstream/pterodactyl-panel/LICENSE.md` and `upstream/pterodactyl-wings/LICENSE` preserved verbatim. Chromium `LICENSE` verified present at the pinned revision upstream (will be preserved when source is imported). |

Phase 5 is therefore *complete for the architecture phase*: no removals
were performed that require validation, and all *future* removal
candidates are documented with their trigger conditions and validation
requirements so the implementation phase can execute them safely.
