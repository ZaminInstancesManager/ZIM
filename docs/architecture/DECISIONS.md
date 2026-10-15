# Architectural Decisions (Phase 6)

This document records ZIM's architectural decisions: the
alternatives considered, the rationale, and the unresolved
questions. It is the canonical ADR (Architecture Decision Record)
index for ZIM.

Each decision is numbered (AD-N) and dated. Once a decision is
made, it is not silently revised; a revision is a new ADR that
supersedes the old one, with a forward reference.

## 1. Settled decisions

### AD-1: ZIM is a desktop product, not a hosted panel.

**Date:** 2026-10-13.
**Status:** Settled.
**Context:** The existing ZIM repository description ("a desktop
panel, CLI, and resident daemon for running and managing Minecraft
servers") and the comprehensive prompt's product identity
("Chromium + Pterodactyl hard fork ... browser-native interface for
managing game servers") describe the same product from different
angles. The product is a desktop application the user installs on
their machine, not a hosted control panel.
**Alternatives considered:**
- Hosted panel (like Pterodactyl Panel itself). Rejected: the
  prompt explicitly mandates a browser-native desktop product.
- Hybrid (desktop shell + hosted panel). Rejected: introduces two
  sources of truth for server state.
**Decision:** ZIM is a desktop product. The browser is the
desktop shell; the engine is the resident daemon.
**Consequences:** No web-server deployment artifacts (the Panel's
Dockerfile, docker-compose, Nix flake are not used in ZIM's
runtime, per `docs/audits/COMPONENT_DISPOSITION.md` §2.5).

### AD-2: Chromium is the browser foundation. (SUPERSEDED by AD-20 — do not implement)

**Date:** 2026-10-13.
**Status:** Settled.
**Context:** The prompt specifies Chromium by name. Alternatives
exist (Firefox/Gecko, WebKit, Servo) but are not under
consideration.
**Alternatives considered:**
- Firefox / Gecko. Rejected: the prompt mandates Chromium.
- WebKit. Rejected: same.
- Servo. Rejected: not production-ready for a full browser.
**Decision:** Chromium is the browser foundation. The pinned
revision is `157.0.8103.0` (commit
`b0a05135bf5a60bb3ab5a1e86a0e55193cb126d0`); see
`chromium/UPSTREAM_MANIFEST.md`.
**Consequences:** ZIM carries Chromium's license obligations
(BSD-style); ZIM inherits Chromium's security model (sandbox, site
isolation); ZIM's build infrastructure depends on `depot_tools` /
`gclient` / `gn` / `ninja`.

### AD-3: The engine is Rust.

**Date:** 2026-10-13.
**Status:** Settled.
**Context:** The prompt mandates Rust. Wings is in Go; the
Panel is in PHP; the engine replaces both. Rust is chosen for
memory safety, performance, and a strong type system that helps
enforce the lifecycle invariants.
**Alternatives considered:**
- Go (matching Wings). Rejected: the prompt mandates Rust.
- Node.js / TypeScript. Rejected: same.
- PHP (matching Panel). Rejected: same.
**Decision:** The engine is Rust. The workspace structure is
proposed in `docs/architecture/RUST_ENGINE.md` §1.
**Consequences:** No PHP or Go runtime in ZIM's final
architecture. The Rust toolchain is a build dependency (blocker
B1 in `docs/architecture/IMPLEMENTATION_ROADMAP.md`).

### AD-4: Browser and engine are separate OS processes.

**Date:** 2026-10-13.
**Status:** Settled.
**Context:** The prompt's "non-negotiable product vision" mandates
that the engine remains operational if Chromium crashes, a
renderer exits, a frontend package fails to load, or a user closes
every browser window. This requires the engine to be a separate
process.
**Alternatives considered:**
- In-process engine (linked into the browser). Rejected: a browser
  crash would stop the engine.
- Engine as a Chromium service (in-process via Mojo). Rejected:
  same.
**Decision:** The engine is a separate OS process, communicating
with the browser via HTTPS+WSS on loopback.
**Consequences:** The engine has its own lifecycle (managed by the
OS: Windows Service, systemd, launchd). The browser and engine can
be updated independently. The engine survives browser crashes.

### AD-5: The engine's API is HTTPS+WSS on loopback.

**Date:** 2026-10-13.
**Status:** Settled.
**Context:** The browser-side integration layer (C++ in the
Chromium browser process) needs to talk to the engine. The choice
of transport affects security, debuggability, and cross-platform
support.
**Alternatives considered:**
- Named pipes / Unix-domain sockets. Rejected (for default):
  cross-platform inconsistency (named pipes on Windows, UDS on
  Unix); debugging is harder (no curl). Adopted as an *optional*
  transport on platforms that support it well, with HTTPS-over-UDS
  as a documented fallback.
- Mojo IPC. Rejected: requires the engine to be in-process,
  violating AD-4.
- gRPC. Rejected: heavier than needed; HTTPS+WSS suffices and
  gives us a documented, debuggable surface (curl, browser dev
  tools).
- DBus. Rejected: Linux-only.
**Decision:** The engine listens on `127.0.0.1:{port}` HTTPS
plus WSS. The browser-side integration layer uses Chromium's
`network::SimpleURLLoader` and WebSocket implementation to talk
to the engine.
**Consequences:** The engine has a documented API
(`docs/api/ZIM_API_SPEC.md`) that's testable with curl. DNS
rebinding must be mitigated (Host header validation, per
`docs/security/THREAT_MODEL.md` §2.1).

### AD-6: The frontend is replaceable via a package manifest.

**Date:** 2026-10-13.
**Status:** Settled.
**Context:** The prompt mandates a fully-replaceable frontend.
**Alternatives considered:**
- Hardcoded default frontend. Rejected: violates the prompt.
- Theme-only replacement. Rejected: insufficient (themes don't
  replace pages).
- Frontend-package replacement with full-replacement option.
  Selected: matches the prompt's three-level customization
  (themes, packages, full replacement).
**Decision:** The frontend is a package loaded by the engine's
frontend-loader. The manifest declares page surfaces,
permissions, signature, and `full_replacement` flag.
**Consequences:** The default frontend is bundled with the
engine and cannot be removed; it is the recovery frontend. A
broken custom frontend cannot lock the user out (invariant 16
in `docs/architecture/ZIM_ARCHITECTURE.md` §4).

### AD-7: ZIM extensions are distinct from Chromium extensions. (SUPERSEDED by AD-20 — moot: no Chromium extension runtime exists)

**Date:** 2026-10-13.
**Status:** Settled.
**Context:** The prompt requires that Chromium extensions do not
auto-inherit ZIM server-control permissions. ZIM needs an addon
ecosystem separate from Chromium's MV3 platform.
**Alternatives considered:**
- Reuse Chromium MV3 for ZIM addons, with a `zim` manifest key
  that grants ZIM permissions. Rejected (as the sole mechanism):
  the MV3 manifest is a *declaration*, not an enforcement point;
  ZIM permissions need engine-boundary enforcement.
- Separate ZIM addon ecosystem, with the engine validating
  permissions on every API call. Selected.
**Decision:** ZIM addons are a separate ecosystem. A ZIM addon
declares permissions in its manifest; the engine validates each
API call against its capability store.
**Consequences:** A compromised Chromium MV3 extension cannot
control ZIM servers without the user explicitly bridging it (which
ZIM does not provide by default).

### AD-8: The Pterodactyl Panel/Wings split is replaced by a single engine.

**Date:** 2026-10-13.
**Status:** Settled.
**Context:** Pterodactyl's architecture has a Panel (PHP) that
issues commands to Wings (Go) via a Panel↔Wings API. ZIM has no
such split.
**Alternatives considered:**
- Transitional dual-backend (ZIM engine + Wings during
  migration). Rejected: the prompt forbids a second independent
  backend.
- Single engine that owns both ends. Selected.
**Decision:** The ZIM engine owns both the client surface (the
HTTPS API the browser calls) and the runtime surface (the
`Runtime` trait that manages containers). There is no
Panel↔Wings-style split.
**Consequences:** The Panel↔Wings API contract is replaced by
the engine's internal node-runtime contract for remote nodes
(see `docs/architecture/RUST_ENGINE.md` §9). The legacy
Panel↔Wings API is preserved only as an importer reference.

### AD-9: Chromium source import is deferred until a build environment with sufficient capacity is provisioned. (CANCELLED by AD-20 — the import will not happen)

**Date:** 2026-10-13.
**Status:** Settled (this assignment).
**Context:** The current workspace has ~9 GB disk, 4 GB RAM, no
`depot_tools`, no Rust toolchain. A full Chromium source + deps
checkout is ~100 GB; a Chromium build requires tens of GB of
memory. The prompt forbids presenting an incomplete checkout as a
fork.
**Alternatives considered:**
- Attempt a partial/blobless clone of chromium/chromium on GitHub
  and commit the resulting sparse subset as the "Chromium fork".
  Rejected: this would be a partial fork mislabeled as complete.
- Document the pin and import strategy, perform source inspection
  via the chromium/chromium GitHub mirror during the audit, defer
  the actual subtree import to the implementation phase. Selected.
**Decision:** Chromium source is not committed to the ZIM repo
in the architecture phase. The pin, import strategy, and
verification status are recorded in
`chromium/UPSTREAM_MANIFEST.md`. The Chromium architecture audit
is performed via spot-fetches against the pinned revision.
**Consequences:** Blockers B2, B3, B4 in
`docs/architecture/IMPLEMENTATION_ROADMAP.md` must be cleared
before milestone M12 (Chromium source import).

### AD-10: Pterodactyl Panel/Wings source is preserved as audit reference, not used in runtime.

**Date:** 2026-10-13.
**Status:** Settled.
**Context:** The Pterodactyl subtrees are imported via `git
subtree` (history-preserving) at pinned revisions
(`v1.15.1`/`v1.13.3`). They contain the source for the behavior
the Rust engine must reproduce.
**Alternatives considered:**
- Remove the Panel/Wings source after extracting the behavioral
  contracts. Rejected: would destroy the audit evidence base
  before the Rust engine is mature.
- Keep the Panel/Wings source as audit reference, not used in
  runtime. Selected.
**Decision:** `upstream/pterodactyl-panel/` and
`upstream/pterodactyl-wings/` are preserved as audit reference.
ZIM does not build them, does not run them, and does not modify
them. Removal candidates are recorded in
`docs/audits/REMOVALS.md` §deferred.
**Consequences:** The ZIM repo's `git log` contains the full
Pterodactyl Panel and Wings history (reachable via the subtree
merge commits). ZIM's own commits do not modify files under
`upstream/`.

### AD-11: The four-state lifecycle model is preserved; ZIM extends with `crash_loop` and `unknown`.

**Date:** 2026-10-13.
**Status:** Settled.
**Context:** Wings uses four process states (`offline`,
`starting`, `running`, `stopping`) and four crash detection
states. The prompt's status model uses four primary visual
statuses (green/yellow/red/blue).
**Alternatives considered:**
- Strictly preserve Wings's four-state model. Rejected: doesn't
  distinguish crash-loop (repeated crashes) or unknown (runtime
  unreachable / node disconnected).
- Replace with a new state model. Rejected: breaks compatibility
  with existing tooling.
**Decision:** ZIM preserves Wings's four states as the primary
states; adds `crash_loop` (after N crashes in window T, per
`docs/architecture/RUST_ENGINE.md` §4) and `unknown` (when the
runtime is unreachable). The visual mapping is documented in
`docs/architecture/BROWSER_INTEGRATION.md` §3.

### AD-12: The Permission vocabulary is preserved verbatim from Pterodactyl.

**Date:** 2026-10-13.
**Status:** Settled.
**Context:** The Panel's `Permission` constants (`websocket.connect`,
`control.console`, `control.start`, etc.) are a known vocabulary
consumed by existing tooling.
**Alternatives considered:**
- Replace with a new vocabulary. Rejected: breaks compatibility.
- Preserve the names, map to a Rust enum. Selected.
**Decision:** The `PermissionGrant` enum in
`docs/architecture/RUST_ENGINE.md` §3 preserves the Panel's
vocabulary verbatim. ZIM may add new grants (e.g. `Install`,
`Transfer`, `AdminInstance`) but does not rename existing ones.

### AD-13: ZIM is a general workload-management platform, not a Minecraft-specific product.

**Date:** 2026-10-13 (expanded scope).
**Status:** Settled.
**Context:** The expanded product vision §1 makes ZIM broader than
a Minecraft/game-server manager. Minecraft is an important initial
workload and Pterodactyl is a major source of reference
functionality, but neither defines the permanent boundaries of
the product.
**Alternatives considered:**
- Keep ZIM as a Minecraft/game-server manager (original scope).
  Rejected: the expanded scope explicitly forbids this constraint.
- Generalize immediately to all workload types (game servers,
  databases, websites, dev processes, generic containers,
  future kinds). Selected.
**Decision:** ZIM is a general workload-management platform. The
canonical Workload abstraction is in
`docs/architecture/WORKLOAD_ABSTRACTION.md`. The architecture
acceptance criteria are in
`docs/architecture/ACCEPTANCE_CRITERIA.md`.
**Consequences:** The engine defines a `Workload` type that wraps
the Pterodactyl-compatible `Instance` type; workload kinds beyond
`GameServer` (Database, Website, DevProcess, GenericContainer,
Custom) are first-class. The original `Instance` type is preserved
for migration compatibility.

### AD-14: The `Runtime` trait is split into a base trait + optional capability traits; unsupported operations report `Unsupported` explicitly.

**Date:** 2026-10-13 (expanded scope).
**Status:** Settled.
**Context:** The original `Runtime` trait (in
`docs/architecture/RUST_ENGINE.md` §4) assumed a console-accepting,
single-process, single-config workload. This is correct for game
servers but too narrow for databases, websites, and other
workload kinds. The expanded scope §2 explicitly lists the
assumptions that must NOT hold.
**Alternatives considered:**
- One trait per workload kind (DatabaseRuntime, WebsiteRuntime,
  etc.). Rejected: proliferates traits and prevents cross-kind
  capability composition.
- A single fat trait with all possible methods, returning Option
  or Result for unsupported ones. Rejected: no compile-time
  guarantee that a runtime claiming a capability actually
  implements it.
- Base `Runtime` trait + optional capability traits
  (`ConsoleCapable`, `ReadyProbeCapable`, `HealthCheckCapable`,
  `FilesystemCapable`, `NetworkCapable`, `SnapshotCapable`,
  `Migratable`, `ExecCapable`, `MetricsCapable`). Selected.
**Decision:** The `Runtime` base trait exposes the minimum
(state, start, stop, kill, generation, kind). Capability traits
are optional; a runtime implements the ones its workload kinds
require. Capability discovery happens at placement time;
unsupported operations at runtime return
`RuntimeError::Unsupported { reason }`, not a silent no-op.
**Consequences:** A workload integration declares its required
capabilities in its manifest; the engine refuses to place the
workload on a runtime that does not provide them. The capability
trait inventory is in `docs/architecture/WORKLOAD_ABSTRACTION.md`
§4 and `docs/architecture/RUST_ENGINE.md` §12.

### AD-15: Workload integrations are loaded as Rust libraries; first-party integrations are build-time-gated, third-party integrations are dynamically loaded.

**Date:** 2026-10-13 (expanded scope).
**Status:** Settled.
**Context:** Workload integrations contain kind-specific logic
(Minecraft's EULA handling, Postgres's initdb, etc.). They must
be loadable into the engine process without requiring a separate
runtime or process boundary.
**Alternatives considered:**
- All integrations dynamically loaded (including first-party).
  Rejected: complicates the first-release build; first-party
  integrations are stable and benefit from being compiled into
  the engine binary.
- All integrations build-time-gated (no dynamic loading).
  Rejected: third-party integrations would require rebuilding
  the engine, which is not acceptable for end users.
- First-party build-time-gated, third-party dynamically loaded.
  Selected.
**Decision:** First-party integrations (`minecraft`, `database`,
`website`, `dev_process`, `generic_container`) live under
`zim/engine/crates/workloads/` and are feature-gated at build
time (a build can include/exclude any subset). Third-party
integrations are dynamically loaded via the integration package
manifest (`zim.toml`); the engine uses `libloading` to load the
integration's `.so` / `.dll` / `.dylib`.
**Consequences:** The engine has a stable ABI for integration
loading; the integration SDK (`zim_engine_workloads_sdk`)
provides the ABI stability guarantees. Third-party integrations
must be compiled against the same ZIM engine major version; the
`api_version` field in the manifest is checked at load time.

### AD-16: First-party workload integrations are bundled under `zim/engine/crates/workloads/`; third-party integrations are loaded via the integration package manifest.

**Date:** 2026-10-13 (expanded scope).
**Status:** Settled.
**Context:** The engine needs a clear separation between
ZaminMC-owned integrations and third-party integrations.
**Alternatives considered:**
- All integrations in a single registry crate. Rejected: couples
  first-party release cadence to third-party submission.
- First-party in `zim/engine/crates/workloads/`, third-party
  loaded via manifest. Selected.
**Decision:** First-party integrations live under
`zim/engine/crates/workloads/{minecraft, database, website,
dev_process, generic_container}/`. Third-party integrations are
packaged as `zim.toml` integration packages and loaded at engine
startup or via `POST /api/v1/workload-integrations` (admin only).
**Consequences:** The first-party integrations are versioned with
the engine; third-party integrations have their own version
number. A first-party integration's `api_version` always matches
the engine's `api_version`; a third-party integration's
`api_version` is checked at load time.

### AD-17: The `Workload` type wraps `Instance` rather than replacing it, preserving the Pterodactyl migration contract.

**Date:** 2026-10-13 (expanded scope).
**Status:** Settled.
**Context:** The expanded scope introduces the `Workload`
abstraction. The original `Instance` type (Pterodactyl-compatible)
is the target of the migration importer. Replacing `Instance`
with `Workload` would break the migration contract.
**Alternatives considered:**
- Replace `Instance` with `Workload`. Rejected: breaks
  Pterodactyl migration importer.
- Keep `Instance` and `Workload` as parallel types. Rejected:
  forces a parallel data model.
- `Workload` wraps `Instance` (Workload has an `instance_id`
  foreign key to Instance). Selected.
**Decision:** A `Workload` row references an `Instance` row. The
Instance carries Pterodactyl-shape fields; the Workload carries
the kind, the spec, the runtime kind, and the declared
capabilities. For workload kinds where a Pterodactyl-specific
field doesn't apply, the field is nullable.
**Consequences:** The migration importer is unchanged
(Panel `servers` → ZIM `instances`); a `Workload` row of kind
`GameServer` is auto-created for each migrated Instance.

### AD-18: Capability discovery is enforced at workload placement time; the engine refuses to place a workload on a runtime that does not provide its required capabilities.

**Date:** 2026-10-13 (expanded scope).
**Status:** Settled.
**Context:** The capability-trait model (AD-14) needs an
enforcement point. Without enforcement, a workload could be
placed on an incompatible runtime and fail at runtime.
**Alternatives considered:**
- Enforce only at runtime (return `Unsupported` on each call).
  Rejected: surfaces failures too late; user has already created
  the workload.
- Enforce at placement time (refuse to create the workload on
  an incompatible runtime). Selected.
- Enforce at both placement and runtime. Selected (defense in
  depth).
**Decision:** At workload creation, the engine inspects the
runtime's `capabilities()` (returns `CapabilitySet`) and the
workload's `required_capabilities` (declared by the integration).
If the runtime's set is not a superset of the required set, the
placement fails with `runtime_incompatible`. At runtime, every
capability call also checks (defense in depth).
**Consequences:** A workload requiring `ConsoleCapable` cannot be
placed on a runtime that does not implement `ConsoleCapable`
(e.g. a static-website runtime). The user gets a structured error
explaining the incompatibility.

### AD-19: The expanded `zim://workloads/{id}/...` URL inventory is the canonical surface; `zim://servers/{id}/...` is a back-compat alias.

**Date:** 2026-10-13 (expanded scope).
**Status:** Settled.
**Context:** The original `zim://servers/{id}/...` URL inventory
(in `docs/architecture/BROWSER_INTEGRATION.md` §2) is
server-flavored. The expanded scope generalizes to workloads.
**Alternatives considered:**
- Replace `zim://servers/{id}/...` with `zim://workloads/{id}/...`.
  Rejected: breaks existing user bookmarks and shared links.
- Keep `zim://servers/{id}/...` as canonical. Rejected: misleading
  for non-server workloads.
- `zim://workloads/{id}/...` canonical; `zim://servers/{id}/...`
  as a back-compat alias that maps to the same handlers. Selected.
**Decision:** The canonical URL inventory is
`zim://workloads/{id}/...`. The legacy `zim://servers/{id}/...`
URLs are accepted by the WebUI handler as aliases; the handler
internally rewrites them to the workload form. New user-facing
documentation and UI use the workload form.
**Consequences:** Existing user bookmarks and shared links
continue to work. The browser-side integration layer treats
both forms identically.

### AD-20: ZIM is a Chrome-inspired desktop application, not a browser. (2026-10-15)

**Status:** Accepted; supersedes AD-2 (Chromium foundation), AD-7
(extension-platform distinction), AD-9 (deferred Chromium import).

**Context:** The original architecture treated the Chrome-inspired UI
requirement as authorization to fork Chromium: source import, a C++
patch queue, browser-process integration, and a 100 GB build
environment. The product requirement was the *interaction model* —
tabs, an address bar, split views, windows, a new-tab experience,
customization — not the browser engine. The correction (2026-10-15)
reframes ZIM as a desktop Minecraft server manager (generalizable to
other workloads per AD-13) with a Chrome-inspired frontend and the
existing Rust engine.

**Decision:**

1. ZIM is a desktop application; the frontend is
   HTML/CSS/TypeScript hosted in a system-WebView desktop shell
   (AD-21).
2. Chrome's UX is the design reference the frontend *replicates* —
   tab strip behavior, animations, split view, omnibox patterns,
   NTP grammar — implemented in our own code, never vendored from
   Chromium source.
3. No Chromium source is imported, built, patched, or shipped. No
   `chromium/PATCHES/` queue. No depot_tools / 100 GB build. The
   `chromium/UPSTREAM_MANIFEST.md` import is cancelled (kept as
   provenance); `docs/audits/CHROMIUM_ARCHITECTURE.md` is reframed
   as a UX/platform reference.
4. ZIM does not implement website browsing, browser history, browser
   bookmarks, a Chromium extension runtime, or browser security
   infrastructure. ZIM extensions are the engine's own addon system.
5. The engine work (milestones M1-M19) is unaffected: the engine
   remains the authority for workload state, and the
   frontend-package model (AD-6) is preserved.

**Consequences:** Roadmap M12-M15 are cancelled and superseded by
M20-M26 (desktop shell, Chrome-UX parity, packaging, release).
AD-7's "distinct from Chromium extensions" distinction is moot (no
Chromium extension runtime exists). Q1-Q3 resolve as moot. The
system WebView is the only browser-engine component ZIM touches —
as an OS-provided runtime, exactly like any other desktop app.

### AD-21: The desktop shell is Tauri 2 over the system WebView. (2026-10-15)

**Status:** Accepted (framework selection; the two open hosting
questions are M21 spikes, see AD-22 and BROWSER_INTEGRATION.md §13).

**Context:** The frontend must be a desktop application hosting the
engine-served, dependency-free TypeScript frontend
(`zim/frontend/`) with windowing, tabs, split views, and OS
integration. Framework candidates:

| Candidate | Verdict | Why |
|-----------|---------|-----|
| Electron | Rejected | Ships a full Chromium runtime inside every install; contradicts the corrected product shape and bloats binaries; its Node backend duplicates engine responsibilities. |
| Per-platform native UI (Qt / Swift / WinUI) | Rejected | Three codebases; no reuse of the engine-served HTML/TS frontend; the highest cost to reach Chrome-UX parity fidelity. |
| Tauri 2 (wry: WebView2 / WKWebView / WebKitGTK) | **Selected** | Rust-native (matches the engine toolchain), renders via the OS webview (no browser engine to build or patch), small binaries, custom-protocol support for `zim://`, multi-window support. |

**Decision:** Build the shell as a Tauri 2 application under
`zim/shell/`. The shell stays thin: windowing, OS integration, and
app chrome (tab strip, address bar, split containers) only. All
management UI remains the engine-served frontend package, preserving
AD-6 (replaceable frontend) and the engine's authority.

**Risks / mitigations:** Webview divergence across platforms —
mitigated by the frontend being dependency-free Web Components (the
existing package) plus a per-platform UI test floor (M22 open
question). The tab/pane hosting strategy (iframe panes vs. multiple
native webviews) is an M21 spike, not a pre-commitment.

### AD-22: `zim://` is an app-internal navigation namespace served by the engine's frontend loader. (2026-10-15)

**Status:** Accepted.

**Context:** The URL inventory (`zim://newtab`,
`zim://servers/{id}/...`, the AD-19 workload form) predates the
correction and remains the correct addressing model for tabs,
session restore, split pairings, and extension links. What changed
is resolution: not a Chromium WebUI scheme, but the shell mapping
the namespace onto the engine-served frontend.

**Decision:** The shell resolves `zim://` URLs to the engine's
active frontend package — via a Tauri custom protocol or a
loopback-origin mapping (the M21 spike decides; tracked in
`docs/architecture/BROWSER_INTEGRATION.md` §13). URLs are stable
identifiers and remain the persistence format for sessions and
quick-access entries.

## 2. Unresolved questions

These are open architectural questions that need further
investigation or a future decision. They are tracked here so they
are not lost; each has a "preliminary" answer that may be revised.

### Q1: The exact `zim://` scheme registration mechanism. (RESOLVED-MOOT by AD-20/AD-22: shell-level resolution, not Chromium WebUI)

**Status:** INVESTIGATE.
**Preliminary:** Custom scheme (separate from `chrome://`), for
clearer privilege separation. Alternative: sub-path under
`chrome://` (e.g. `chrome://zim/...`) which requires fewer
Chromium patches.
**Investigation needed:** Read the full `content/public/browser/web_ui.h`
at the pinned Chromium revision; understand how Chromium
distinguishes trusted vs untrusted WebUI; confirm the scheme's
CSP behavior.

### Q2: The exact `TabFeatures` attachment API. (RESOLVED-MOOT by AD-20: no Chromium; per-tab state is frontend state)

**Status:** INVESTIGATE.
**Preliminary:** `ServerTabFeature` implements the `TabFeature`
interface declared in `chrome/browser/ui/tabs/tab_features.h`.
**Investigation needed:** Read the full `tab_features.h` to
confirm the attachment lifecycle, the per-tab feature
construction parameters, and the destruction semantics.

### Q3: The exact `BrowserWindowFeatures` API surface. (RESOLVED-MOOT by AD-20: no Chromium; shell windowing is Tauri's)

**Status:** INVESTIGATE.
**Preliminary:** ZIM contributes window-scoped features via
`BrowserWindowFeatures`.
**Investigation needed:** Read `chrome/browser/ui/browser_window/`
at the pinned revision; confirm the registration mechanism.

### Q4: Initial database choice (SQLite vs. Postgres).

**Status:** Open.
**Preliminary:** SQLite for single-machine (default); Postgres
optional for multi-node deployments where a single authoritative
DB is preferable to per-node SQLite.
**Decision needed:** confirm SQLite's WAL-mode durability is
sufficient; decide whether the first release ships Postgres
support or defers it.

### Q5: Remote-node contract wire format (gRPC vs. HTTPS+WSS).

**Status:** Open.
**Preliminary:** HTTPS+WSS (consistent with the engine's local
API surface, no separate binary protocol).
**Decision needed:** confirm HTTPS+WSS's overhead is acceptable
for the node-runtime traffic (heartbeat + state changes +
console output + metrics); if too high, switch to gRPC.

### Q6: The `zim.toml` server-definition format's exact schema.

**Status:** Open.
**Preliminary:** TOML with typed variables, signed definitions,
declared permissions. The legacy egg importer is the
compatibility bridge.
**Decision needed:** finalize the schema in `zim/contracts/`
during M9.

### Q7: Frontend package signature algorithm.

**Status:** Open.
**Preliminary:** Ed25519.
**Decision needed:** confirm the chosen Rust crate's stability;
decide on a trust-anchor rotation mechanism.

### Q8: OAuth provider set.

**Status:** Open.
**Preliminary:** pluggable via a config file, with Discord first
(matches the gaming audience).
**Decision needed:** confirm Discord OAuth scopes; decide on
second provider (Google / GitHub).

### Q9: Remote-node mutual authentication mechanism.

**Status:** Open.
**Preliminary:** mTLS with node-scoped certificates issued by the
engine. Alternative: signed JWT with rotating shared secret.
**Decision needed:** confirm mTLS is feasible in Rust without
excessive dependency weight; decide on certificate authority
mechanism.

### Q10: Whether to ship WebAuthn (hardware security key) login.

**Status:** Open.
**Preliminary:** yes, as an opt-in for admins.
**Decision needed:** confirm the Rust WebAuthn crate's maturity.

### Q11: Audit log retention policy.

**Status:** Open.
**Preliminary:** 365 days default, configurable.
**Decision needed:** confirm regulatory requirements (none
specific to ZIM's audience, but admins in some jurisdictions may
have requirements).

### Q12: Whether to support Linux without systemd (e.g. Alpine with OpenRC).

**Status:** Open.
**Preliminary:** yes; the engine ships a generic "launcher" mode
that does not depend on systemd, with reduced auto-start
reliability.
**Decision needed:** confirm the launcher mode's behavior on each
non-systemd init system.

## 3. Decision-making discipline

ZIM's decisions are made by the project owner (the GitHub user
`Abasing`). A decision is recorded in this document when:

- The question is settled (preliminary becomes decision).
- The context and rationale are documented.
- The alternatives considered are recorded.
- The consequences are surfaced.

A decision is revised by superseding it with a new ADR (AD-N+1)
that explicitly references the superseded ADR. The old ADR's status
changes to "Superseded by AD-N+1".

## 4. Cross-references

- Audit documents: `docs/audits/`
- Architecture documents: `docs/architecture/`
- API documents: `docs/api/`
- Security: `docs/security/`
- Compatibility: `docs/compatibility/`
- Decisions (this document): `docs/architecture/DECISIONS.md`

Where two documents disagree, the precedence is:

1. `docs/api/ZIM_API_SPEC.md` for API contracts.
2. `docs/architecture/RUST_ENGINE.md` for engine internals.
3. `docs/architecture/BROWSER_INTEGRATION.md` for browser
   integration.
4. `docs/architecture/ZIM_ARCHITECTURE.md` for cross-component
   boundaries and lifecycle invariants.
5. `docs/architecture/DECISIONS.md` (this document) for
   rationale and open questions.
6. Audit documents for evidence.

The audit documents are *evidence*, not contracts; if an audit
document and an architecture document disagree, the architecture
document is canonical (and the audit document should be updated).
