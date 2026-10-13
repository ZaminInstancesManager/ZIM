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

### AD-2: Chromium is the browser foundation.

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

### AD-7: ZIM extensions are distinct from Chromium extensions.

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

### AD-9: Chromium source import is deferred until a build environment with sufficient capacity is provisioned.

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

## 2. Unresolved questions

These are open architectural questions that need further
investigation or a future decision. They are tracked here so they
are not lost; each has a "preliminary" answer that may be revised.

### Q1: The exact `zim://` scheme registration mechanism.

**Status:** INVESTIGATE.
**Preliminary:** Custom scheme (separate from `chrome://`), for
clearer privilege separation. Alternative: sub-path under
`chrome://` (e.g. `chrome://zim/...`) which requires fewer
Chromium patches.
**Investigation needed:** Read the full `content/public/browser/web_ui.h`
at the pinned Chromium revision; understand how Chromium
distinguishes trusted vs untrusted WebUI; confirm the scheme's
CSP behavior.

### Q2: The exact `TabFeatures` attachment API.

**Status:** INVESTIGATE.
**Preliminary:** `ServerTabFeature` implements the `TabFeature`
interface declared in `chrome/browser/ui/tabs/tab_features.h`.
**Investigation needed:** Read the full `tab_features.h` to
confirm the attachment lifecycle, the per-tab feature
construction parameters, and the destruction semantics.

### Q3: The exact `BrowserWindowFeatures` API surface.

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
