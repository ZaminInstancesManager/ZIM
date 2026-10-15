# The ZIM Addon SDK (M24)

This document is the addon author's contract: the manifest, the
package format, the trust model, the serving model, and the
vendored `zim.js` client. The engine-side enforcement lives in
`zim/engine/crates/extensions/` (spec:
`docs/api/FRONTEND_EXTENSION_SPEC.md` §4; decisions: AD-7, AD-29).

A working, complete example ships at
`zim/extensions/samples/hello-zim/` — one page surface, one sidebar
item, one context-menu item, one command, one event subscription,
the vendored SDK. Start there.

## 1. What an addon is

A ZIM addon is a ZIP package (`.zimpkg`) with a `zim.addon.json`
manifest at the root, uploaded by an admin
(`POST /api/v1/extensions`, `Content-Type: application/zip`). It is
NOT a Chromium MV3 extension — a separate ecosystem (AD-7) with
its own permission vocabulary, its own trust model, and no
marketplace (§4: manual install only).

Install requirements, all enforced engine-side:

- The manifest validates against the §4.1 schema (below).
- `api_version` matches the engine's generation by major (`v1` ≡
  `v1.0.0`).
- `engines.zim` (a semver range) matches the running engine.
- The signature classifies as trusted, untrusted, or unsigned — a
  *trusted key whose signature fails to verify* is a hard reject no
  admin approval can override.
- Every `dependencies` entry (`id@range`) is satisfied by an
  installed addon or the active frontend package — otherwise the
  install fails with nothing written (§4.9, no partial installs).
- No `page_surfaces` entry overlaps the active frontend's or another
  addon's (§4.4), and `zim://ext/…` is reserved for the engine's
  addon-asset namespace.

The admin's upload IS the approval for untrusted/unsigned packages
(§4.11); the permission set the manifest declares is granted at
install, listed at `GET /api/v1/extensions`, extendable via
`POST /api/v1/extensions/{id}/grant` (admin — §4.2's elevation
re-approval), and revoked at uninstall.

## 2. The manifest (`zim.addon.json`)

```json
{
  "$schema": "https://zim.zaminmc.org/schemas/addon.v1.json",
  "manifest_version": 1,
  "id": "org.example.zim-addon.database-manager",
  "version": "0.1.0",
  "name": "Database Manager",
  "description": "Adds a database management UI to ZIM.",
  "author": "Example Author",
  "license": "MIT",
  "api_version": "v1",
  "engines": { "zim": ">=0.1.0" },
  "permissions": ["database.read", "database.view_password"],
  "instance_scope": "optional",
  "page_surfaces": ["zim://servers/{id}/databases"],
  "entrypoints": { "databases": "pages/databases.html" },
  "sidebar_items": [
    { "label": "Databases", "icon": "database", "page": "zim://servers/{id}/databases" }
  ],
  "context_menu_items": [
    { "label": "Backup database", "page": "zim://servers/{id}/databases?action=backup" }
  ],
  "commands": [
    { "id": "create-database", "label": "Create database", "accelerator": "Ctrl+Shift+D" }
  ],
  "event_subscriptions": ["instance.state_change", "job.completed"],
  "dependencies": ["org.zaminmc.frontend.default@>=1.0.0"]
}
```

Field rules (the engine's validator, in one place):

| Field | Rules |
|---|---|
| `id` | reverse-DNS (ASCII letters, digits, dots, hyphens; ≥1 dot). Bundled defaults are `org.zaminmc.extension.*` — disable-only (§6). |
| `manifest_version` | must be `1`. |
| `version` | semver; updates must be strictly greater (downgrades refused). |
| `api_version` | the API generation, matched by major. |
| `engines.zim` | a semver range against the running engine. |
| `permissions` | the closed engine vocabulary (`PermissionGrant::as_str`: `websocket.connect`, `control.*`, `file.*`, `backup.*`, `allocation.*`, `user.*`, `schedule.*`, `database.*`, `settings.*`, `install`, `transfer`, `admin.instance`, `workload.*`, `network.*`); no duplicates. |
| `instance_scope` | `"required"` (per-instance), `"optional"` (per-instance-opt-in), or `"none"` (global). |
| `page_surfaces` | well-formed `zim://` patterns; `{param}` segments match one concrete segment; each surface routes to an entrypoint keyed by its **last non-parameter segment** (`servers/{id}/databases` → `databases`); no surface may overlap another package's or live under `zim://ext/`. |
| `entrypoints` | `pages/…html` package-relative paths (confinement-checked: no `..`, no absolute paths, no backslashes); every declared file must exist in the package. |
| `sidebar_items` / `context_menu_items` | `page` must be one of the declared surfaces (the context-menu target may append `?query`). |
| `commands` | unique slug `id`s; `accelerator` = `Modifier+…+Key` (Ctrl/Cmd/Alt/Shift + letter/digit/F1-F12); **reserved and rejected**: Ctrl/Cmd+T, +N, +W and their Shift forms (§4.12 — the shell owns the tab/window lifecycle). |
| `event_subscriptions` | the engine's closed set: `instance.state_change`, `instance.resource_usage`, `instance.console_output`, `instance.install_progress`, `job.completed`, `job.progress`; no duplicates. |
| `dependencies` | `id@range` entries; no self-dependency; unsatisfiable = install failure. |
| `signature` | optional; the §3.4 block (`ed25519`, base64 32-byte key, base64 64-byte signature over the canonical manifest — `signature` removed, keys sorted). |

Package limits: ≤ 32 MiB, ≤ 1000 files, ≤ 32 MiB per entry.

## 3. The serving model

- An enabled addon's **declared surfaces** serve at
  `https://127.0.0.1:{port}/zim/{surface-path}` — the same
  loopback-origin mapping every `zim://` URL uses (AD-23). A
  disabled addon's surfaces answer 404; enabling restores them.
- The addon's **own files** (stylesheets, scripts, images) serve
  under the reserved namespace
  `/zim/ext/{addon-id}/{file}` — e.g.
  `<script src="/zim/ext/org.example.zim-addon.x/assets/app.js">`.
  Only enabled addons' namespaces serve.
- Every addon response carries the same §3.8 CSP as the default
  frontend's pages (`default-src 'none'; script-src 'self'; …`,
  plus M22's `frame-src 'self'` / `frame-ancestors 'self'` and M23's
  `ipc:` connect entries), the same nosniff/referrer headers, and
  digest-ETag revalidation. **Write CSP-clean pages**: external
  scripts and stylesheets only, no inline handlers, no
  `javascript:` URLs.
- **Error isolation (§4.10, adapted post-AD-20 — AD-29)**: the
  spec's per-origin process isolation language predates the
  Chromium cancellation. In ZIM's webview model all pages are
  same-origin on the loopback origin, so the engine's isolation is
  *disable-with-notice*: a package whose on-disk manifest or tree
  fails to load at boot is disabled, its notice surfaces at
  `zim://extensions`, and every other addon plus the default
  frontend keep working. The frontend's per-pane iframe boundaries
  (AD-24) contain page-level failures.
- **Permissions at the boundary (§4.2, adapted)**: addon pages call
  the same `/api/v1` surface as the default frontend's pages, with
  the same session credentials — the engine's per-endpoint authz
  (invariant 15) gates every call. The manifest's permission set is
  granted/listed/revocable and enforced client-side by this SDK
  (`Zim.assertPermission`, `requiredPermission`); server-side
  per-addon attribution on the shared loopback origin is an M25
  hardening item (see the roadmap's honest-boundary notes).

## 4. The vendored client (`zim.js`)

Author in plain JS/TS against one global: `Zim`. Vendor the SDK
into your package (`assets/zim.js` — the repo's packaging script
does this for you, §5) and load it before your page script:

```html
<script src="/zim/ext/{your-addon-id}/assets/zim.js"></script>
<script src="/zim/ext/{your-addon-id}/assets/app.js"></script>
```

```js
// app.js — the complete tour.
const self = await Zim.init("org.example.zim-addon.x");   // resolves the registration
const me = await Zim.whoami();                            // null when signed out
const instance = await Zim.api("/api/v1/instances/" + id);// §6-shaped errors (ApiError)

// Gated call: throws before any request leaves the page when the
// addon lacks the grant (UX, not security — the engine re-checks).
await Zim.api("/api/v1/instances/" + id + "/power", {
  method: "POST",
  body: { operation: "restart" },
  requiredPermission: "control.restart",
});

// One §11 stream per instance: sub-protocol zim-events.v1,
// exponential backoff, replay-from-checkpoint.
const stream = new Zim.ZimStream(instanceId, {
  onEvent: (event) => { /* your manifest's event_subscriptions */ },
  onGap: () => { /* re-fetch over HTTP, never trust a partial replay */ },
  onFatal: (reason) => { /* surface it honestly */ },
});
window.addEventListener("beforeunload", () => stream.close());
```

API surface:

- `Zim.init(addonId)` → `Promise<SelfSummary | null>` — resolves
  the addon's registration (granted permissions, enabled flag,
  contributions). Idempotent.
- `Zim.api(path, options?)` → `Promise<T>` — same-origin,
  cookie-credentialed fetch; `options`: `method`, `body` (JSON),
  `query`, `requiredPermission`. Errors throw `Zim.ApiError`
  (`code`, `status`, `field`, `requestId`, `missingPermissions`,
  `.isAuth`, `.isPermission`).
- `Zim.assertPermission(name)` — the client-side gate,
  standalone.
- `Zim.whoami()` → `Promise<Me | null>`.
- `Zim.ZimStream` — the §11 subscription (constructor takes
  `instanceId` + handlers; `.close()` on teardown).

## 5. Packaging

The repo's helper (Node ≥ 20, `zip` on PATH):

```sh
node zim/extensions/scripts/package-addon.mjs validate <addon-dir>
node zim/extensions/scripts/package-addon.mjs build <addon-dir> out.zimpkg
```

`build` whitelists `zim.addon.json` + `pages/` + `assets/`, vendors
`zim/sdk/addon/dist/zim.js` → `assets/zim.js` (when not already
vendored), and zips a `.zimpkg`. `validate` is the fast author-time
gate over the same rules the engine enforces (§2's table).

Install on a running engine:

```sh
curl -k -X POST "https://127.0.0.1:{port}/api/v1/extensions" \
  -H "Cookie: zim_session={admin-session}" \
  -H "Content-Type: application/zip" \
  --data-binary @out.zimpkg
```

Then manage it: `POST /api/v1/extensions/{id}/enable|disable`,
`POST /api/v1/extensions/{id}/grant` (`{"permissions": [...]}`),
`DELETE /api/v1/extensions/{id}` — admin, audited; the management
UI is `zim://extensions` in the default frontend.

## 6. What addons cannot do (§4.12)

Themes, frontend packages, and addons cannot replace the tab
strip's behavior, the omnibox, the window chrome, or the reserved
shortcuts (Ctrl+T/N/W and their Shift forms — rejected at manifest
validation). A `commands` entry adds a shortcut; it never overrides
one. Command semantics in the default frontend: the accelerator
navigates the active tab to the addon's first declared surface
(`{id}` resolved against the active server tab — an honest no-op
toast when none is active).

## 7. Trust and signing

The signature block and trust semantics are shared with frontend
packages (§3.4): ed25519 over the canonical manifest JSON, against
the engine's trust store (the built-in ZaminMC development key plus
`{data_dir}/extensions/trusted-keys.json`, same file format as the
frontend's). Classification: `trusted` (auto-installable),
`invalid` (hard reject), `untrusted`/`unsigned` (installable under
admin approval — which an admin's upload is). The repo-held
development seed (`zim/engine/crates/frontend-loader/keys/`) signs
test fixtures; production keys rotate out-of-band.
