# ZIM Threat Model (Phase 6)

> **CORRECTION (2026-10-15, AD-20):** Trust tiers and scenarios
> involving the Chromium browser process, renderer isolation, or a
> browser-extension attack surface are superseded by the
> desktop-shell model (AD-20/AD-21): the frontend attack surface is
> the OS webview runtime plus the engine's loopback API. Re-rating
> of affected scenarios is tracked in the M21 milestone;
> engine-side scenarios are unaffected.

This document threat-models the entire ZIM product: trust boundaries,
attack scenarios, mitigations, and outstanding security questions. It
is canonical for ZIM's security architecture; other documents reference
it rather than re-stating the threat analysis.

It does not cover Chromium's own security model (sandboxing, site
isolation, V8 sandbox) — that is upstream and preserved. It covers
the security properties *ZIM introduces* by combining Chromium with an
engine that manages game servers and exposes their state to the
browser.

## 1. Trust boundaries

ZIM has six distinct trust tiers, each with a different capability
surface:

| Tier | What it is                                              | Capabilities                                                                                                | Enforcement |
|------|---------------------------------------------------------|-------------------------------------------------------------------------------------------------------------|--------------|
| 1. Ordinary web content | Pages on the open web loaded in a tab.                | Chromium's normal web platform; no ZIM access.                                                              | Chromium sandbox + same-origin policy + CSP. |
| 2. Chromium MV3 extensions | Browser extensions installed via Chromium's extension platform. | Their declared MV3 permissions; **no automatic ZIM permission**.                                       | Chromium's extension permission model. |
| 3. Trusted ZIM internal pages | Pages at `zim://` (newtab, settings, servers/{id}/...). | The ZIM API surface, scoped to the page's capability grant from the engine.                              | Engine's authz at API boundary; Chromium's WebUI CSP; site-isolation (own renderer process per `zim://` host). |
| 4. ZIM management extensions | Addons with a `zim` manifest key.                     | The ZIM permissions declared in their manifest, validated against the engine's capability store on every API call. | Engine's authz on every API call; signature check on install. |
| 5. Native browser shell | The Chromium browser process.                          | Full browser authority.                                                                                      | Chromium's existing browser-process privilege. |
| 6. Rust engine | The engine process.                                    | Full engine authority (server lifecycle, files, DB, network).                                              | OS-level: runs as the user, no root/admin; loopback-only listener; remote-node contract uses authenticated channel. |

The key invariant is that *manifests are not proof of authorization*.
An extension or frontend package *declares* what it wants; the engine
*checks* on every API call. A compromised manifest cannot bypass the
engine boundary because the manifest is not the enforcement point.

## 2. Attack scenarios

### 2.1 Malicious web content

**Scenario:** A user visits a malicious web page in a tab. The page
attempts to interact with the engine.

**Mitigations:**
- The engine listens on loopback only. The web page's origin is
  not loopback; same-origin policy blocks cross-origin requests
  to the engine.
- The engine requires a valid credential on every request. A web
  page cannot forge a session cookie (HttpOnly, Secure, SameSite=Strict,
  scoped to loopback origin) or an API key (not in the page's origin).
- CORS: the engine does not issue CORS allowances to any origin
  other than its own loopback origin. Preflight requests from web
  origins fail.
- The engine does not expose any "no-auth" endpoint that returns
  sensitive data; the only no-auth endpoint is `/api/v1/version` (which
  returns only the engine version string).

**Residual risk:** DNS rebinding (the attacker controls DNS for
their domain and rebinds it to 127.0.0.1 mid-session). Mitigation:
the engine validates the `Host` header on every request and rejects
any `Host` other than `127.0.0.1` / `localhost` / a configured
loopback hostname. (Equivalent to the mitigation used by Syncthing,
Docker, and other loopback services.)

### 2.2 Compromised renderer

**Scenario:** A renderer process is compromised (e.g. via a V8
exploit delivered through web content). The attacker escapes the
renderer sandbox.

**Mitigations:**
- A `zim://` page's renderer is site-isolated from web renderers
  (separate process, separate origin). A web-renderer compromise
  does not directly compromise a `zim://` renderer.
- Even a compromised `zim://` renderer does not have arbitrary
  engine access: it has the page's capability token, scoped to
  that tab's permissions. The attacker cannot use the tab token to
  issue operations outside the tab's scope.
- The engine does not trust the renderer for any privileged
  decision. Even a fully-compromised renderer can only issue API
  calls within the tab's permission set; the engine re-checks
  every API call.

**Residual risk:** A compromised renderer with full process
compromise could read the tab's capability token from memory and
use it for the token's lifetime. Mitigation: tab tokens are
short-lived (refreshed on tab focus) and revoked on tab close.

### 2.3 Malicious extensions (Chromium MV3)

**Scenario:** A Chromium MV3 extension installed by the user
attempts to control ZIM servers.

**Mitigations:**
- A Chromium MV3 extension does not have access to `zim://` pages
  by default (Chromium's MV3 host permissions govern this). Even
  with `host_permissions: ["zim://*/*"]`, the extension cannot
  forge a tab capability token (the token is issued by the engine
  to a specific tab session, validated on every API call).
- A Chromium extension cannot auto-inherit ZIM permissions. The
  ZIM permission model is separate from Chromium's MV3 permission
  model; the two are not bridged.

**Residual risk:** A malicious Chromium extension with broad
host permissions could inject a content script into a `zim://`
page and issue API calls from the page's context. This is
mitigated by:
- `zim://` pages declare a strict CSP that blocks inline script
  and only allows scripts from the page's own package origin.
  A content script from a Chromium extension is still allowed by
  Chromium's content-script mechanism (CSP does not block
  user-installed extension content scripts), but the page can
  detect the injected script via standard detection techniques.
- The engine ships with a documented list of trusted Chromium
  extensions (none by default) and an admin can configure a
  blocklist of disallowed extensions.

### 2.4 Compromised frontend packages

**Scenario:** A user installs a malicious frontend package that
attempts to exfiltrate server data or issue unauthorized operations.

**Mitigations:**
- Frontend packages must be signed by a trusted author. The engine
  ships with a curated set of trusted author public keys. A package
  signed by an unknown author requires explicit admin approval
  (with a warning) to install.
- A frontend package's permissions are admin-approved on install.
  A package cannot elevate its permissions silently; permission
  elevation requires admin re-approval.
- The package's pages run under a strict CSP that allows loading
  assets only from the package's own origin and the engine's API
  origin. Inline script is forbidden. `eval`, `Function`
  constructor, and dynamic external script loading are blocked.
- A frontend package cannot make cross-origin network requests to
  arbitrary hosts (CSP blocks `connect-src` to non-loopback
  origins); exfiltration requires a separate request to an
  attacker-controlled host, which CSP blocks.

**Residual risk:** A compromised trusted author's signing key.
Mitigation: trusted-author keys are rotated; revocation lists
are surfaced to the engine on a documented schedule; compromised
packages are blocklisted.

### 2.5 Malicious server definitions and installation scripts

**Scenario:** A user imports a Pterodactyl egg with a malicious
install script. The script attempts to escape the container or
exfiltrate data.

**Mitigations:**
- Egg install scripts run inside a constrained Docker container
  with:
  - A non-root user.
  - A read-only root filesystem (where possible).
  - A network policy that blocks outbound traffic by default
    (admin can configure exceptions per egg).
  - A CPU / memory / disk limit.
  - A deadline (configurable; default 1 hour).
  - A mount of the server's data directory only; no mounts of
    engine-privileged paths.
- Eggs that declare `priviledged_install_scripts: true` are
  **rejected by default** and require explicit admin opt-in per
  egg. The opt-in is per-egg, not per-instance; a malicious
  privileged egg is a documented risk that the admin assumes.
- The egg importer parses the egg JSON defensively; malformed
  JSON, path traversal in startup command templates, and
  oversized fields are rejected at import time.

**Residual risk:** Container escape vulnerabilities in Docker
itself. Mitigation: ZIM does not run containers as root; the
engine runs the Docker client as the user, and containers run
as a non-root user. ZIM tracks Docker security advisories and
recommends the user run a recent Docker version.

### 2.6 Path traversal and symbolic-link attacks

**Scenario:** A malicious request to the file API attempts to
access paths outside the instance's data directory via `../`
sequences or symbolic links.

**Mitigations:**
- The file API resolves the requested path against the instance's
  data directory *and verifies the resolved path is within that
  directory*. Path traversal via `../` is detected and rejected
  with `400 bad_request`.
- The file API follows symlinks only if the symlink's target is
  within the instance's data directory. Symlinks pointing outside
  are rejected.
- Archive extraction (zip, tar) is path-traversal-aware: entries
  with paths containing `../` or starting with `/` are rejected,
  not extracted.
- The instance's data directory itself is the only path the file
  API accepts; absolute paths in requests are rejected.

**Residual risk:** Race conditions between the path check and the
file access (TOCTOU). Mitigation: use `openat2` with
`RESOLVE_BENEATH` on Linux 5.6+ (this is the documented
defense-in-depth mechanism); on other platforms, the file API
uses `realpath` and re-validates after resolution. A test
verifies TOCTOU resistance (see `docs/architecture/TEST_STRATEGY.md`).

### 2.7 Archive extraction attacks

**Scenario:** A user uploads a malicious archive (zip bomb,
overlapping entries, deeply-nested paths).

**Mitigations:**
- The file API's archive extraction has a configurable maximum
  uncompressed size (default 5 GB). An archive that would exceed
  the limit is rejected.
- The extraction has a maximum entry count (default 100k entries).
- The extraction has a maximum nesting depth for archives
  containing archives (default 3).
- Overlapping entries (a path that an earlier entry already
  created) are rejected.
- Symbolic-link entries whose targets are outside the extraction
  directory are rejected.
- The extraction runs in a constrained subprocess (sandboxed
  via `seccomp` on Linux) with a CPU and memory limit.

**Residual risk:** A novel archive format with a parser
vulnerability. Mitigation: ZIM uses well-tested archive libraries
(`zip-rs`, `tar`) and rejects formats those libraries don't
support.

### 2.8 Credential leakage

**Scenario:** A user's session cookie or API key is leaked (e.g. via
a malicious extension, a log file, a crash dump).

**Mitigations:**
- Session cookies are `HttpOnly`, `Secure`, `SameSite=Strict`,
  scoped to the loopback origin. A malicious page cannot read
  them via JavaScript.
- API keys are issued with a short default lifetime (90 days)
  and can be revoked at any time. A leaked key is revocable.
- Tab capability tokens are short-lived (refreshed on tab focus)
  and revoked on tab close. A leaked tab token has a very short
  window of validity.
- The engine does not log credentials. Audit log entries include
  the `actor_id` (user ID) but not the credential.
- Crash dumps do not include credential values (Chromium's crash
  key mechanism is used for ZIM-specific keys; sensitive fields
  are not added as crash keys).

**Residual risk:** A memory-dumping attacker with process
compromise. Mitigation: OS-level defense (the user's machine is
presumed to be free of malware; if it isn't, all bets are off —
this is the standard posture for any desktop application).

### 2.9 Unauthorized console commands

**Scenario:** A user without `control.console` permission attempts
to send console input to an instance.

**Mitigations:**
- The engine's authz layer checks `control.console` on every
  `POST /api/v1/instances/{id}/console/input` request. A request
  without the permission is rejected with `403 permission_denied`.
- The authz check is at the API boundary; the streaming module
  does not re-check (it's called only by the API layer).
- A tab capability token includes the `control.console` grant
  only if the tab's user has that permission; a tab without the
  grant cannot send console input even if its JS is compromised.

**Residual risk:** None material — the check is centralized and
enforced by the engine.

### 2.10 Cross-site requests to local engine endpoints

This is the same scenario as §2.1 (malicious web content). The
mitigations are the same.

### 2.11 Local privilege escalation

**Scenario:** An attacker with access to the user's account
attempts to escalate to root/admin via ZIM.

**Mitigations:**
- The engine does not require root/admin. It runs as the user.
  The Docker client runs as the user (with the user in the
  `docker` group on Linux; this is a documented Docker constraint,
  not a ZIM weakness).
- The engine's data directory is owned by the user with `0600`
  permissions on sensitive files (the DB, secrets store).
- The engine does not write to system paths; all its data is under
  a single user-owned directory.
- The engine's updater does not auto-elevate; updates require
  user consent (the user runs the updater themselves).

**Residual risk:** The Docker group on Linux is effectively
root-equivalent (a member of the `docker` group can mount the
host filesystem). This is a known Docker posture; ZIM documents
this and recommends running Docker in rootless mode where possible.

### 2.12 Remote-node compromise

**Scenario:** A remote Linux node running ZIM's node-runtime is
compromised. The attacker uses the node to attack the central
engine.

**Mitigations:**
- The remote node authenticates to the engine with a node-scoped
  capability token (rotated periodically). A compromised node's
  token can be revoked.
- The engine treats remote-node reports as *observed state*, not
  as authoritative. The engine's authoritative state (instances,
  users, permissions, jobs) lives on the engine; a compromised
  node cannot rewrite the engine's authoritative state.
- The remote node runs its own runtime (Docker, etc.) in its own
  process. A compromised node's runtime escapes affect only
  instances on that node, not the central engine.
- The remote-node contract is versioned; a version mismatch puts
  the node in `version_mismatch` state and its instances in
  `Unknown` until resolved.

**Residual risk:** A compromised node could send false observed
state (e.g. claim an instance is running when it isn't, or vice
versa). The engine's reconciliation logic (on engine restart)
verifies observed state against the runtime; a lying node would
be detected by the heartbeat + version + generation checks
during reconciliation.

### 2.13 Resource exhaustion

**Scenario:** A user creates many instances, runs many jobs, or
uploads many large files, exhausting disk or memory.

**Mitigations:**
- Per-user instance limits (configurable; default 50 instances per
  non-admin user).
- Per-instance resource limits (CPU, memory, disk) — enforced by
  the runtime (Docker cgroups).
- Per-instance backup count + size limits (configurable).
- Per-instance schedule count limit (default 50).
- Engine-wide job concurrency limit (default 10 concurrent jobs).
- Disk-full detection: the engine refuses new operations with
  `resource_exhausted` when disk free falls below a threshold.

**Residual risk:** A user with `admin` permission can configure
very high limits. Mitigation: admin actions are audit-logged;
a misconfigured admin is detectable.

### 2.14 Malformed protocol URLs

**Scenario:** A user clicks a `zim://` URL with a malformed
structure (e.g. `zim://servers/not-a-uuid/console`).

**Mitigations:**
- The WebUI handler validates the URL structure against the
  documented `zim://` URL inventory (see
  `docs/architecture/BROWSER_INTEGRATION.md` §2). Malformed URLs
  return a 404 internal page.
- Missing server IDs return a "this server does not exist" page.
- Unauthorized `zim://` URLs (e.g. `zim://settings` accessed
  without admin) return a permission-denied page.

**Residual risk:** None material.

### 2.15 Dependency compromise

**Scenario:** A ZIM dependency (Rust crate, JS package, Chromium
vendored code) is compromised upstream.

**Mitigations:**
- Rust dependencies are pinned in `Cargo.lock`; the lockfile is
  committed to the repo. Updates are reviewed.
- The engine's `Cargo.toml` uses `cargo-vendor` to vendor
  dependencies; the vendored tree is committed (configurable).
- A `cargo audit` step runs in CI to detect known CVEs in
  dependencies.
- Chromium's vendored `third_party/` is preserved as-is; ZIM
  tracks upstream Chromium security advisories on each re-pin.
- Frontend package dependencies (npm) are pinned in
  `package-lock.json` and `npm audit` runs in CI.

**Residual risk:** A novel compromise of a dependency that has
no CVE yet. Mitigation: defense-in-depth — even a compromised
dependency is constrained by the engine's authz layer (the
dependency cannot bypass the API boundary).

### 2.16 Update and rollback attacks

**Scenario:** An attacker distributes a malicious update to a
frontend package or extension that downgrades a previously-patched
version.

**Mitigations:**
- Frontend packages and addons must be signed by a trusted author.
- The engine records the highest-seen version of each package and
  refuses to downgrade below it without explicit admin opt-in.
- ZIM's own binary updates are signed; the updater refuses
  unsigned binaries.

**Residual risk:** A compromised signing key. Mitigation: trusted
keys are rotated; revocation lists are documented.

## 3. Explicit capability boundaries

The prompt requires explicit capability boundaries for each trust
tier. These are the canonical boundaries:

- **Ordinary web content:** can render pages and use the standard
  web platform. **Cannot** access the ZIM engine. **Cannot** read
  or write files on the user's machine. **Cannot** execute
  processes.
- **Chromium MV3 extensions:** can do whatever their declared MV3
  permissions allow. **Cannot** automatically control ZIM
  servers. **Cannot** auto-inherit ZIM permissions.
- **Trusted ZIM internal pages:** can call the ZIM API within
  their capability grant. **Cannot** make arbitrary network
  requests (CSP-restricted). **Cannot** execute arbitrary
  processes.
- **ZIM management extensions (addons):** can call the ZIM API
  within their declared permissions. **Cannot** bypass the engine's
  authz. **Cannot** make arbitrary network requests (CSP-restricted).
- **Native browser shell:** can do whatever Chromium can do. (Not
  a ZIM-controlled boundary; it's Chromium's.)
- **Rust engine:** full engine authority. **Cannot** execute
  arbitrary user-shell commands (the engine does not expose a
  shell-exec API; all runtime operations go through the
  `Runtime` trait, which is constrained to container or
  host-process execution per the instance's configured runtime
  kind).

## 4. Authentication and authorization details

- **Local IPC auth:** loopback HTTPS with `Host` header validation
  (mitigates DNS rebinding). Tab capability tokens are scoped to a
  tab session and revoked on tab close.
- **WebSocket auth:** `Authorization: Bearer {token}` on WSS
  upgrade; origin check (only loopback origin allowed).
- **Origin restrictions:** the engine accepts requests only from
  its loopback origin; CORS is not enabled for any other origin.
- **Secret storage:** API keys are stored as Argon2id hashes in
  the engine's DB (not plaintext). Session secrets are random
  256-bit tokens, stored in memory only. Tab capability tokens
  are random 256-bit tokens, stored in memory only.
- **Revocation:** API keys are revocable via `DELETE
  /api/v1/users/{id}/api-keys/{keyId}`. Sessions are revocable
  via `POST /api/v1/auth/logout`. Tab tokens auto-revoke on tab
  close. Frontend packages and addons can be disabled by admin.
- **Audit behavior:** every privileged operation is audit-logged
  with the actor, action, target instance, and timestamp. The
  audit log is append-only (no update or delete API).

## 5. Outstanding security questions

These are security-relevant design questions not yet settled:

1. **The remote-node contract's exact mutual authentication
   mechanism.** Preliminary: mTLS with node-scoped certificates
   issued by the engine. Alternative: signed JWT with rotating
   shared secret.
2. **The exact frontend package signature algorithm.** Preliminary:
   Ed25519. Alternative: RSA-PSS.
3. **Whether ZIM should ship a hardware-security-key (WebAuthn)
   login option.** Preliminary: yes, as an opt-in for admins.
4. **Whether the engine should support a "kiosk" mode where the
   user is auto-logged-in to a single instance's console without
   password.** Preliminary: no (security posture: every session
   is authenticated).
5. **The exact audit log retention policy.** Preliminary: 365 days
   default, configurable.
6. **Whether the engine should support TLS 1.3 client-cert
   authentication for the loopback listener.** Preliminary: yes
   for the remote-node contract; no for the loopback listener
   (overkill for loopback).
7. **The exact Docker network policy.** Preliminary: instances
   are on an isolated bridge network by default; per-instance
   firewall rules allow only the configured port allocations.

These questions are tracked in `docs/architecture/DECISIONS.md`
§unresolved and are not blockers for the architecture phase.
