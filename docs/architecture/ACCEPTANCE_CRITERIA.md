# Architecture Acceptance Criteria (Phase 6 expansion)

> **CORRECTION (2026-10-15, AD-20):** Acceptance criteria that
> gate on a built ZIM browser binary are superseded by the
> desktop-shell criteria in the corrected roadmap (M21-M26).
> Engine-side criteria are unaffected.

This document demonstrates, with reference to the existing
architecture documents, how ZIM's architecture satisfies each of
the 10 expanded-scope acceptance criteria (from the "Additional
Master Requirements for ZIM" §12).

Each criterion is restated, then demonstrated by reference to the
specific architecture decisions, types, and contracts that satisfy
it. Where the criterion is satisfied by the original architecture
(pre-expansion), the original document is cited. Where the expansion
introduced new architecture to satisfy it, the new document is cited.

These are *architecture* acceptance criteria, not implementation
milestones. The implementation roadmap
(`docs/architecture/IMPLEMENTATION_ROADMAP.md`) tracks when each
criterion is *verified* by a working build + tests; this document
tracks that the *architecture* supports each criterion.

## Criterion 1: Browse ordinary websites using Chromium's existing
browser capabilities

**Satisfied by:** the original architecture's preservation of
Chromium's browser infrastructure.

**Evidence:**

- `docs/audits/COMPONENT_DISPOSITION.md` §1.1–1.14 marks every
  required Chromium browser component (browser process, tab strip
  model, session restore, omnibox, bookmarks, history, NTP
  replacement, internal WebUI pages, extension system, site
  isolation, network service, build system, test infrastructure)
  as **KEEP**.
- `docs/architecture/ZIM_ARCHITECTURE.md` §2 documents that the
  browser process is Chromium-derived and preserved as-is.
- `docs/architecture/BROWSER_INTEGRATION.md` §11 explicitly
  enumerates what the integration layer does NOT do (does not
  modify the omnibox, address bar, bookmarks, history, etc.) —
  ordinary web browsing is preserved.
- `docs/security/THREAT_MODEL.md` §2.1 confirms that ordinary web
  content runs in Chromium's normal sandbox with no ZIM engine
  access.

**Architecture acceptance:** verified. The browser is Chromium;
ordinary websites work as in any Chromium-based browser.

## Criterion 2: Open, restore, split, and organize workload tabs
without coupling tab lifetime to workload lifetime

**Satisfied by:** the original architecture's lifecycle invariants
(3, 4, 5, 6) and the expansion's workload-tab model.

**Evidence:**

- `docs/architecture/ZIM_ARCHITECTURE.md` §4 invariants:
  - Invariant 3: "A tab's existence is not proof that a server
    should run." (Generalized in
    `docs/architecture/WORKLOAD_ABSTRACTION.md` §8: "A workload
    tab is a view of a workload, not the workload itself.")
  - Invariant 4: "Closing a tab does not stop its server."
    (Generalized: closing a tab does not stop its workload.)
  - Invariant 5: "Restoring a session does not issue server
    lifecycle commands." (Generalized: restoring a session does
    not issue workload lifecycle commands.)
  - Invariant 6: "Multiple tabs cannot accidentally create
    multiple server processes." (Generalized: multiple tabs of
    the same workload are multiple views, not multiple
    workloads.)
- `docs/architecture/BROWSER_INTEGRATION.md` §3 documents the
  `ServerTabFeature` (now conceptually `WorkloadTabFeature`) that
  attaches workload metadata to a tab via `TabFeatures` —
  the tab does not own the workload, the engine does.
- `docs/architecture/BROWSER_INTEGRATION.md` §7 documents that
  split view uses Chromium 157's existing first-class `split_tabs`
  API — split panes are independent views, each with its own
  `WorkloadTabFeature`; closing one pane does not stop the
  workload.
- `docs/architecture/CHROMIUM_ARCHITECTURE.md` §6.3 documents
  Chromium's session restore infrastructure (session service,
  session service log, exit type service, restore-on-startup
  policy handler) — ZIM preserves this and does not introduce a
  second source of truth for browser state.

**Architecture acceptance:** verified. Tab lifetime is decoupled
from workload lifetime by:
- The engine owns workload state; the browser owns tab state.
- The integration layer attaches workload metadata to tabs via
  `TabFeatures` (no subclass of `WebContents`).
- Closing a tab only unsubscribes that tab's WebSocket connection.
- Session restore re-navigates tabs to `zim://workloads/{id}/...`
  URLs; the loaded pages query the engine for current state.

## Criterion 3: Manage a Minecraft server through a workload-specific
integration

**Satisfied by:** the `minecraft` first-party workload integration
plus the `GameServer` workload kind.

**Evidence:**

- `docs/architecture/WORKLOAD_ABSTRACTION.md` §5 declares the
  first-party `minecraft` integration implementing
  `WorkloadKind::GameServer` for Minecraft/Paper/Fabric/Forge,
  understanding EULAs, server JARs, startup flags, game versions,
  player counts, and Minecraft console conventions.
- `docs/architecture/WORKLOAD_ABSTRACTION.md` §3 declares
  `GameServerSpec` with `egg_id`, `image`, `startup`,
  `variables`, `allocations`, `skip_egg_scripts`,
  `crash_detection_enabled` — the same fields Pterodactyl uses,
  preserved verbatim for compatibility.
- `docs/compatibility/MIGRATION_AND_COMPATIBILITY.md` §3 documents
  the egg importer that reads existing Pterodactyl eggs (including
  the Minecraft egg) and produces a ZIM `GameServerSpec`.
- `docs/architecture/RUST_ENGINE.md` §3 declares `Instance` with
  Pterodactyl-compatible fields; `WORKLOAD_ABSTRACTION.md` §7
  documents that a `Workload` of kind `GameServer` references an
  `Instance` directly.
- `docs/architecture/RUST_ENGINE.md` §4 declares the `Runtime`
  trait; `WORKLOAD_ABSTRACTION.md` §4 declares the `ConsoleCapable`
  capability trait that the Minecraft integration requires (the
  Minecraft server console accepts text commands like `/say`,
  `/stop`, etc.).

**Architecture acceptance:** verified. A Minecraft server is
managed by:
- A `Workload` row of kind `GameServer`.
- An `Instance` row (Pterodactyl-compat fields).
- A `GameServerSpec` carrying Minecraft-specific config.
- The `minecraft` first-party integration declaring it requires
  `ConsoleCapable`, `FilesystemCapable`, `MetricsCapable`,
  `ExecCapable`.
- The Pterodactyl egg importer preserves the existing Minecraft
  egg's install script and startup template.

## Criterion 4: Manage a non-Minecraft workload without introducing
Minecraft-specific assumptions into the core engine

**Satisfied by:** the workload abstraction's capability trait model
and the typed `WorkloadSpec` enum.

**Evidence:**

- `docs/architecture/WORKLOAD_ABSTRACTION.md` §2 establishes five
  logical layers with explicit boundary rules; the "ZIM engine"
  layer (layer 3) does not contain Minecraft-specific code.
  Minecraft-specific code lives in layer 5 (workload integrations)
  under `zim/engine/crates/workloads/minecraft/`.
- `docs/architecture/WORKLOAD_ABSTRACTION.md` §4 declares the
  capability traits (`ConsoleCapable`, `ReadyProbeCapable`,
  `HealthCheckCapable`, `FilesystemCapable`, `NetworkCapable`,
  `SnapshotCapable`, `Migratable`, `ExecCapable`, `MetricsCapable`).
  These are the engine's vocabulary for *what a runtime can do*,
  independent of *what kind of workload it is*.
- `docs/architecture/WORKLOAD_ABSTRACTION.md` §4 documents that
  capability discovery happens at placement time: a workload
  declares its required capabilities; the engine refuses to place
  the workload on a runtime that does not provide them.
- `docs/architecture/WORKLOAD_ABSTRACTION.md` §4 documents that
  unsupported operations return `Unsupported { reason }`, not a
  silent no-op. A database integration that asks a non-ConsoleCapable
  runtime for console input gets a structured error.
- `docs/architecture/WORKLOAD_ABSTRACTION.md` §11 explicitly lists
  what the abstraction does NOT assume (every workload has a console;
  every workload is one executable; every workload can be stopped
  by a generic signal; every workload can be migrated by file copy;
  every workload uses the same networking; every workload has the
  same install/update procedure).
- A `Database` workload (declared in `WORKLOAD_ABSTRACTION.md` §3)
  has `DatabaseSpec` with `engine`, `version`, `credentials_ref`,
  `init_script_ref`, `readiness_probe`, `backup_strategy`,
  `endpoints` — none of which are Minecraft-shaped. The
  `database` first-party integration declares it requires
  `ReadyProbeCapable`, `FilesystemCapable`, `SnapshotCapable`,
  `NetworkCapable`, `MetricsCapable` — not `ConsoleCapable`.
- The original `Runtime` trait in `RUST_ENGINE.md` §4 is the *base*
  trait (state, start, stop, kill, generation, kind); the capability
  traits are *optional*. A workload integration that doesn't need
  console support doesn't require the runtime to implement
  `ConsoleCapable`.

**Architecture acceptance:** verified. The core engine knows about
`Workload`, `WorkloadKind`, `WorkloadSpec`, the `Runtime` base
trait, and the capability trait set. It does not know about
Minecraft-specific concepts (EULAs, JARs, player counts). Those
live in the `minecraft` integration, loaded via the integration
package manifest, scoped to `WorkloadKind::GameServer`.

## Criterion 5: Expose additional workload types through documented
interfaces

**Satisfied by:** the workload integration package manifest and
the `WorkloadKind::Custom` variant.

**Evidence:**

- `docs/architecture/WORKLOAD_ABSTRACTION.md` §5 documents the
  integration package manifest (`zim.toml`), with
  `manifest_version`, `id`, `version`, `api_version`, `kind`,
  `display_name`, `description`, `required_capabilities`,
  `optional_capabilities`, `permissions`, `config_schema`,
  `runtime_kinds`, `signature`.
- `docs/architecture/WORKLOAD_ABSTRACTION.md` §5 declares that
  third-party workload integrations can declare
  `WorkloadKind::Custom(CustomKindId)` and be loaded via the same
  integration package manifest.
- `docs/architecture/WORKLOAD_ABSTRACTION.md` §5 documents the
  workload integration trust boundary: trusted for Rust memory
  safety (compile-time enforced), untrusted for capability scope
  (runtime-checked by the engine's authz layer).
- `docs/architecture/WORKLOAD_ABSTRACTION.md` §3 declares the
  `WorkloadSpec::Custom(CustomSpec)` variant, which is opaque to
  the engine core; the integration reads and writes it.
- `docs/api/FRONTEND_EXTENSION_SPEC.md` §4 documents the addon
  SDK, including the `permissions` field that an integration
  declares. The workload integration package uses the same
  permission model.

**Architecture acceptance:** verified. A new workload type is
exposed by:
- Authoring a Rust crate that implements the integration's
  `WorkloadIntegration` trait (declares the kind, the spec
  schema, the required capabilities, the install/lifecycle/etc.
  handlers).
- Packaging the integration as a `zim.toml` integration package.
- Signing it with a trusted author key (or getting admin approval).
- Uploading via `POST /api/v1/workload-integrations` (the
  endpoint is documented in
  `docs/api/ZIM_API_SPEC.md` §10.12 as a sub-resource of
  extensions).
- The engine loads the integration, registers the
  `WorkloadKind::Custom(CustomKindId)`, and exposes the new
  workload type to the user.

## Criterion 6: Allow a custom frontend without transferring engine
authority to frontend code

**Satisfied by:** the original architecture's frontend-replacement
model and the engine's authz layer.

**Evidence:**

- `docs/api/FRONTEND_EXTENSION_SPEC.md` §3 documents the frontend
  package manifest with `permissions` (declared, not granted
  silently), `signature` (required), `full_replacement` (explicit
  flag).
- `docs/api/FRONTEND_EXTENSION_SPEC.md` §3.5 documents the smoke
  test that runs before activation.
- `docs/api/FRONTEND_EXTENSION_SPEC.md` §3.6 documents recovery
  to the default frontend on failure — the engine's frontend
  loader (in `docs/architecture/RUST_ENGINE.md` §10) reverts and
  surfaces a recovery notice.
- `docs/security/THREAT_MODEL.md` §2.4 (compromised frontend
  packages) documents that frontend packages run under strict CSP
  that allows loading assets only from the package's own origin
  and the engine's API origin; inline script is forbidden.
- `docs/architecture/ZIM_ARCHITECTURE.md` §4 invariant 16:
  "Restoration of default frontend on custom-frontend failure."
- `docs/architecture/WORKLOAD_ABSTRACTION.md` §2 documents that
  the "ZIM user experience" layer (layer 2) is *not* authoritative
  for workload state. A custom frontend package is part of layer
  2; the engine (layer 3) is the authority. The frontend calls
  the engine via the HTTPS+WSS API; the engine validates every
  call against the caller's capability grant.

**Architecture acceptance:** verified. A custom frontend:
- Cannot bypass the engine's authz layer (every API call is
  checked at the engine boundary).
- Cannot make arbitrary network requests (CSP-restricted).
- Cannot run inline script (CSP-forbidden).
- Cannot lock the user out (the default frontend is always
  available as recovery).

## Criterion 7: Enforce granular permissions across websites,
extensions, frontend packages, and runtime integrations

**Satisfied by:** the original architecture's six trust tiers and
the expanded scope's capability-grant model.

**Evidence:**

- `docs/security/THREAT_MODEL.md` §1 documents the six trust tiers
  (ordinary web content, Chromium MV3 extensions, trusted ZIM
  internal pages, ZIM management extensions, native browser shell,
  Rust engine) with explicit capability boundaries per tier.
- `docs/architecture/RUST_ENGINE.md` §3 declares the
  `PermissionGrant` enum with 40+ grants, preserved verbatim from
  Pterodactyl's `Permission` constants (per
  `docs/audits/PTERODACTYL_ARCHITECTURE.md` §7.1.4) plus ZIM
  additions (`Install`, `Transfer`, `AdminInstance`).
- `docs/architecture/WORKLOAD_ABSTRACTION.md` §9 extends the
  permission vocabulary with workload-generalized grants
  (`workload.read`, `workload.create`, `workload.create.{kind}`,
  `workload.delete`, `workload.migrate`, `workload.snapshot`,
  `workload.deploy`, `network.expose`, `network.unexpose`).
- `docs/architecture/WORKLOAD_ABSTRACTION.md` §5 documents that
  workload integrations are capability-scoped: an integration
  that declares it manages `GameServer` workloads cannot manage
  `Database` workloads without explicit admin grant.
- `docs/security/THREAT_MODEL.md` §3 documents explicit capability
  boundaries for each tier — ordinary web content has zero engine
  access; MV3 extensions do not auto-inherit ZIM permissions;
  trusted ZIM internal pages call the API within their capability
  grant; ZIM management extensions declare permissions checked at
  the engine boundary.
- `docs/architecture/ZIM_ARCHITECTURE.md` §4 invariant 15:
  "Capability checks at the engine boundary, not in the frontend."
- `docs/api/ZIM_API_SPEC.md` §4 documents that every endpoint
  declares the `PermissionGrant` set it requires; the engine's
  authz module checks every required grant against the caller's
  `PermissionSet` and returns `403 permission_denied` with the
  missing grants listed.

**Architecture acceptance:** verified. Granular permissions are
enforced:
- At the engine API boundary (every call checked against the
  caller's `PermissionSet`).
- At the workload integration boundary (integrations are scoped
  to their declared `WorkloadKind`).
- At the runtime capability boundary (capability trait methods
  return `Unsupported` if the runtime doesn't implement them).
- At the frontend CSP boundary (frontend packages cannot load
  scripts from external origins).

## Criterion 8: Recover from engine, browser, process, and
remote-node failures without silently violating workload state

**Satisfied by:** the original architecture's 16 lifecycle
invariants and the failure-behavior matrix.

**Evidence:**

- `docs/architecture/ZIM_ARCHITECTURE.md` §4 invariants 1, 2, 7,
  9, 10, 11, 12, 13, 14 cover engine-restart reconciliation,
  accepted-request vs. started-process distinction, lifecycle
  race prevention, per-instance serialization, partial-failure
  cleanup, deletion/transfer coordination, stale-event
  protection, and crash-loop protection.
- `docs/architecture/ZIM_ARCHITECTURE.md` §5 documents failure
  behavior for: renderer crash, all browser windows closed,
  browser process crash, engine process crash, DB transaction
  failure, remote node disconnect, custom frontend failure,
  custom extension failure, malformed `zim://` URL, missing
  server ID, deleted instance, unauthorized request, container
  runtime unavailable, resource exhaustion, update/rollback
  attack.
- `docs/architecture/RUST_ENGINE.md` §4 documents the
  `CrashState` struct with `last_crash`, `recent_crashes`,
  `in_crash_loop` for crash-loop protection.
- `docs/architecture/RUST_ENGINE.md` §4 documents the
  `generation: AtomicU64` counter bumped on every transition;
  runtime events whose generation is older than the current
  generation are ignored (stale-event protection).
- `docs/architecture/WORKLOAD_ABSTRACTION.md` §4 documents that
  the `Migratable` capability trait has
  `prepare_for_migration()` → `MigrationCheckpoint` and
  `abort_migration(checkpoint)`, so an interrupted migration
  can be rolled back to a known state.
- `docs/architecture/TEST_STRATEGY.md` §2 lists 16 invariant
  tests covering each lifecycle invariant; §3 lists 16
  failure-scenario tests covering engine restart, renderer
  crash, duplicate requests, delayed exit events, interrupted
  backups, failed transfers, invalid extension packages, disk
  full, DB transaction failure, node disconnect, custom frontend
  failure, tab capability token theft, path traversal, archive
  attack, DNS rebinding, cross-origin request.
- `docs/architecture/WORKLOAD_ABSTRACTION.md` §6 documents
  that workload kinds may declare extended lifecycle states
  (Recovering, Initializing, Deploying, Draining, Watching)
  with explicit substate mappings; a workload in `Recovering`
  is not misreported as `Running`.

**Architecture acceptance:** verified. Failure modes are covered
by:
- 16 lifecycle invariants, each with an enforcement point and a
  test.
- 16 failure-scenario tests, each with a documented expected
  behavior.
- The workload abstraction's capability trait model (e.g.
  `Migratable` with explicit checkpoint/abort) ensures
  workload-specific failure modes (interrupted migration,
  interrupted backup) are recoverable.

## Criterion 9: Operate on supported platforms with explicit
runtime capability reporting

**Satisfied by:** the original architecture's platform support
matrix and the workload abstraction's capability discovery.

**Evidence:**

- `docs/architecture/PLATFORM_SUPPORT.md` §1 documents the
  platform matrix: Windows x64, Linux x64/arm64, macOS
  Apple Silicon / Intel, remote Linux node, remote Windows node.
- `docs/architecture/PLATFORM_SUPPORT.md` §3 documents
  isolation capabilities per platform — Linux containers
  (Docker Engine) are first-class; Windows and macOS use Docker
  Desktop's Linux VM; host runtime is opt-in admin-consent per
  instance (Linux only).
- `docs/architecture/PLATFORM_SUPPORT.md` §8 explicitly states:
  "Do not claim equivalent container support across operating
  systems without evidence." The asymmetry is documented:
  Linux is first-class for hosting; Windows and macOS host via
  Docker Desktop's VM; ZIM does not pretend Windows-native
  containers or macOS-native containers provide equivalent
  isolation.
- `docs/architecture/WORKLOAD_ABSTRACTION.md` §4 documents that
  runtime adapters report their `capabilities()` (the set of
  capability traits they implement) at registration; the engine
  refuses to place a workload on a runtime that does not provide
  the workload's required capabilities.
- `docs/architecture/WORKLOAD_ABSTRACTION.md` §5 documents that
  integration packages declare `runtime_kinds = ["Container",
  "Remote"]` (or whatever subset applies); the engine refuses to
  place a workload of a kind whose integration declares it
  requires `Container` runtime on a platform that doesn't provide
  container support (e.g. a workload requiring Linux containers
  on a platform that doesn't provide Docker).

**Architecture acceptance:** verified. The architecture:
- Does not claim equivalent container support across operating
  systems.
- Reports runtime capabilities explicitly via the `Runtime::capabilities()`
  method and the integration package's `runtime_kinds` field.
- Refuses to place workloads on incompatible runtimes (the engine
  returns `runtime_incompatible` rather than silently degrading).

## Criterion 10: Upgrade the browser and management engine while
preserving user data and maintaining a documented compatibility
strategy

**Satisfied by:** the original architecture's update strategy and
the API versioning contract.

**Evidence:**

- `docs/api/ZIM_API_SPEC.md` §2 documents API versioning by URL
  prefix (`/api/v1`, `/api/v2`); additive changes don't bump
  the version; breaking changes do.
- `docs/architecture/PLATFORM_SUPPORT.md` §9 documents update and
  rollback strategy per platform: engine updates via Windows
  Service / Linux apt/yum / macOS Sparkle; browser (Chromium
  fork) updates as a single binary; frontend package updates via
  the engine's frontend-loader (rollback supported by setting
  the active package to a previous version); egg updates via
  `POST /api/v1/eggs/{id}` (rollback supported by re-importing
  a previous version); extension updates by uploading a new
  version (rollback supported by setting the active version).
- `docs/architecture/UPSTREAM_MAINTENANCE.md` documents the
  Chromium re-pin cadence (4-week, matching Chromium stable),
  the local patch queue under `chromium/PATCHES/`, and the
  patch resolution log under §3.5.
- `docs/architecture/UPSTREAM_MAINTENANCE.md` §3.6 documents
  the patch backport policy: "always forward" — no backports
  to older ZIM revisions; users update to the latest ZIM release
  for security fixes.
- `docs/architecture/RUST_ENGINE.md` §5 documents that
  migrations are forward-only SQL; no down-migrations. ZIM's
  own metadata is small; a rollback-via-restore is safer than
  a schema rollback.
- `docs/compatibility/MIGRATION_AND_COMPATIBILITY.md` documents
  the migration importer for users coming from Pterodactyl,
  preserving user data (servers, allocations, schedules,
  subusers, etc.).
- `docs/security/THREAT_MODEL.md` §2.16 (update and rollback
  attacks) documents that the engine records the highest-seen
  version of each frontend/extension package and refuses to
  downgrade below it without explicit admin opt-in (mitigates
  rollback attacks).
- `docs/architecture/WORKLOAD_ABSTRACTION.md` §5 documents that
  workload integration packages have an `api_version` field
  (the engine's API version they target); the engine refuses
  to load an integration whose declared `api_version` is not
  supported.

**Architecture acceptance:** verified. The architecture:
- Versions the API explicitly (`v1`, `v2`).
- Updates browser and engine independently (separate processes,
  separate binaries, separate update cadences).
- Preserves user data on update (forward-only migrations;
  migration importer for Pterodactyl-to-ZIM).
- Documents rollback strategy (rollback-via-restore for ZIM
  metadata; re-import for eggs and extensions; revert active
  frontend package on failure).
- Mitigates rollback attacks (highest-seen version tracking;
  admin opt-in for downgrade).

## Summary

All 10 expanded-scope acceptance criteria are satisfied by the
architecture. The original architecture documents (Phase 6) cover
criteria 1, 2, 6, 7, 8, 9, 10 substantially. The expansion
introduced in `docs/architecture/WORKLOAD_ABSTRACTION.md` and the
related updates to `docs/architecture/ZIM_ARCHITECTURE.md` §9 and
`docs/architecture/RUST_ENGINE.md` §12 cover criteria 3, 4, 5
(the workload abstraction layer).

**Architecture acceptance is complete.** Implementation acceptance
(as opposed to architecture acceptance) is tracked in
`docs/architecture/IMPLEMENTATION_ROADMAP.md`; the first
implementation milestone M2 (Rust engine skeleton) clears the way
for the rest.

This document does not claim that any of the above is implemented
and tested. It claims that the *architecture* supports each
criterion, with references to the specific documents and types
that provide the support. The verification status of each
criterion is "Planned" (per the verification discipline in
`docs/architecture/TEST_STRATEGY.md` §7) until the corresponding
implementation milestone reaches "Verified".
