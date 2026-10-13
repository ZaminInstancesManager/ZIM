# Frontend & Extension Specification (Phase 6)

This document specifies the ZIM frontend-replacement model and the
ZIM addon SDK. It is canonical for:

- Frontend package manifest, entry points, API version, capability
  declarations, installation, update, rollback, recovery.
- Themes (color/typography/spacing/icon tokens).
- ZIM addon manifest, package structure, lifecycle, permissions,
  capability grants, activation, dependencies, error isolation,
  removal.
- The three levels of customization: themes, frontend packages,
  full frontend replacement.

It is the contract between the engine's frontend loader (see
`docs/architecture/RUST_ENGINE.md` §10) and any author of a ZIM
frontend or ZIM addon.

## 1. Three levels of customization

ZIM supports three explicitly-different customization levels. Each
has a distinct manifest format, a distinct capability surface, and a
distinct failure/recovery model.

| Level                    | Replaces                                          | Manifest                                    | Capabilities declared | Recovery on failure |
|--------------------------|---------------------------------------------------|---------------------------------------------|------------------------|---------------------|
| Theme                    | Color/typography/spacing/icon tokens              | `zim.theme.json`                            | (none)                 | Revert to previous theme |
| Frontend package         | Management pages (dashboards, navigation, UI)     | `zim.frontend.json`                         | API version + page surfaces | Engine reverts to default frontend; surfaces recovery notice |
| Full frontend replacement | Everything the user sees at `zim://`              | `zim.frontend.json` with `full_replacement: true` | Same as frontend package, plus explicit page list | Same |

A theme can be combined with any frontend package; a frontend
package can be combined with any theme; a full-frontend-replacement
package implies its own theme.

## 2. Themes

### 2.1 Theme manifest

```json
{
  "$schema": "https://zim.zaminmc.org/schemas/theme.v1.json",
  "manifest_version": 1,
  "id": "org.zaminmc.theme.darkcompact",
  "version": "1.0.0",
  "name": "Dark Compact",
  "description": "Compact dark theme for power users.",
  "author": "ZaminMC",
  "tokens": {
    "color.background": "#1a1a1a",
    "color.surface": "#242424",
    "color.text.primary": "#e0e0e0",
    "color.text.secondary": "#a0a0a0",
    "color.accent": "#3b82f6",
    "color.success": "#22c55e",
    "color.warning": "#f59e0b",
    "color.danger": "#ef4444",
    "color.indicator.running": "#22c55e",
    "color.indicator.starting": "#f59e0b",
    "color.indicator.crashed": "#ef4444",
    "color.indicator.stopped": "#3b82f6",
    "color.indicator.unknown": "#9ca3af",
    "typography.body.family": "Inter, system-ui, sans-serif",
    "typography.body.size": "14px",
    "typography.heading.family": "Inter, system-ui, sans-serif",
    "typography.heading.size": "18px",
    "spacing.unit": "8px",
    "radius.small": "4px",
    "radius.medium": "6px",
    "radius.large": "10px",
    "icon.set": "lucide"
  }
}
```

### 2.2 Token vocabulary

Tokens are defined by the engine; a theme that omits a token falls
back to the engine's default for that token. The full token set is
in `zim/contracts/theme-tokens.v1.json`.

A theme cannot introduce new tokens (the engine wouldn't know what
to do with them). It can only override existing ones.

### 2.3 Theme packaging and installation

A theme is a `.zip` archive containing `zim.theme.json` at the
root. Install via `POST /api/v1/themes` (admin only). The engine
validates the manifest, extracts the archive, and stores the theme
in its data directory.

A theme does not contain executable code. Tokens are values, not
scripts; the engine refuses themes whose tokens are not literal
strings/numbers.

### 2.4 Theme failure and recovery

If a theme's manifest is invalid (schema failure, unknown token
type, etc.), the engine refuses to install it. If a previously
installed theme somehow becomes invalid (e.g. by manual file
corruption), the engine reverts to the previous theme on next
load.

## 3. Frontend packages

### 3.1 Frontend package manifest

```json
{
  "$schema": "https://zim.zaminmc.org/schemas/frontend.v1.json",
  "manifest_version": 1,
  "id": "org.zaminmc.frontend.default",
  "version": "1.0.0",
  "name": "ZIM Default Frontend",
  "description": "The default ZIM frontend.",
  "author": "ZaminMC",
  "license": "Apache-2.0",
  "api_version": "v1",
  "engines": { "zim": ">=0.1.0" },
  "full_replacement": false,
  "page_surfaces": [
    "zim://newtab",
    "zim://servers",
    "zim://servers/{id}/console",
    "zim://servers/{id}/files",
    "zim://servers/{id}/settings",
    "zim://servers/{id}/backups",
    "zim://servers/{id}/schedules",
    "zim://servers/{id}/subusers",
    "zim://servers/{id}/metrics",
    "zim://settings"
  ],
  "entrypoints": {
    "newtab": "pages/newtab.html",
    "servers": "pages/servers.html",
    "console": "pages/console.html",
    "files": "pages/files.html",
    "settings": "pages/settings.html",
    "backups": "pages/backups.html",
    "schedules": "pages/schedules.html",
    "subusers": "pages/subusers.html",
    "metrics": "pages/metrics.html"
  },
  "assets": [
    "assets/main.css",
    "assets/console.js",
    "assets/files.js"
  ],
  "permissions": [
    "websocket.connect",
    "control.console",
    "control.start",
    "control.stop",
    "control.restart",
    "file.read",
    "file.read_content",
    "file.write",
    "file.create",
    "file.delete",
    "file.archive",
    "backup.read",
    "backup.create",
    "backup.update",
    "backup.delete",
    "backup.restore",
    "backup.download",
    "schedule.read",
    "schedule.create",
    "schedule.update",
    "schedule.delete",
    "user.read",
    "user.create",
    "user.update",
    "user.delete",
    "settings.read",
    "settings.update",
    "install",
    "transfer",
    "admin.instance"
  ],
  "signature": "..."
}
```

### 3.2 Page surfaces

`page_surfaces` is the explicit list of `zim://` URLs this package
claims to provide. The engine's WebUI handler routes a `zim://` URL
to the package that declares it. A `zim://` URL not declared by
any installed package falls back to the default frontend (or, if
the default doesn't declare it either, returns a 404 internal page).

### 3.3 Permissions

`permissions` is the list of `PermissionGrant` names the package
requests. The engine grants these on install (admin approval
required). Without the grant, the page's API calls to the engine
return `403 permission_denied`.

### 3.4 Signature

A frontend package must be signed by its author. The engine ships
with a curated set of trusted author public keys (ZaminMC's own
key is always trusted). A package signed by an unknown key
requires explicit admin approval to install.

### 3.5 Smoke test

Before activating a new frontend package, the engine runs a
*smoke test*:

1. Load the manifest, validate against schema.
2. Check `engines.zim` is compatible with the engine's version.
3. Check `api_version` is one the engine supports.
4. Load each entrypoint's HTML and verify it parses.
5. Execute a configured smoke-test page (a tiny HTML page that
   loads `zim-api.js` and makes one `GET /api/v1/auth/whoami`
   call); if the smoke-test page can render and receive a response,
   the smoke test passes.
6. If all checks pass, activate; otherwise, refuse and keep the
   previous active package.

### 3.6 Recovery

If the active frontend package fails at runtime (e.g. the engine
detects the package's files are corrupted, or the package crashes
the smoke test on a re-verification), the engine:

1. Reverts to the default frontend package (always bundled, never
   removable).
2. Surfaces a recovery notice on the next `zim://newtab` load.
3. Records the failure in the audit log.

A broken custom frontend therefore cannot lock the user out of ZIM.

### 3.7 Accessibility

A frontend package must declare its accessibility conformance level
in the manifest:

```json
{
  "accessibility": {
    "standard": "WCAG 2.1",
    "level": "AA"
  }
}
```

The engine does not (cannot) verify the package's actual conformance;
the declaration is a contract by the package's author. Packages
that declare a level they don't actually meet are flagged in the
extension management UI when reported by users.

### 3.8 Security restrictions

- CSP: a frontend package's pages run under a strict CSP that
  allows loading assets only from the package's own origin (served
  by the engine) and from the engine's API origin. Inline script
  is forbidden.
- No `eval`, no `Function` constructor, no dynamic script tags
  from external URLs.
- No Web workers that load scripts from external origins.
- Service workers are allowed (for offline behavior) but must be
  registered with the engine's origin and are subject to the same
  CSP.

## 4. ZIM addon SDK (extensions)

ZIM addons are *not* Chromium MV3 extensions. They are a separate
ecosystem. A ZIM addon can be installed by the user, declares
ZIM-specific permissions, and is checked at the engine boundary.

### 4.1 Addon manifest

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
  "permissions": [
    "database.read",
    "database.create",
    "database.update",
    "database.delete",
    "database.view_password"
  ],
  "instance_scope": "optional",
  "page_surfaces": [
    "zim://servers/{id}/databases"
  ],
  "entrypoints": {
    "databases": "pages/databases.html"
  },
  "sidebar_items": [
    { "label": "Databases", "icon": "database", "page": "zim://servers/{id}/databases" }
  ],
  "context_menu_items": [
    { "label": "Backup database", "page": "zim://servers/{id}/databases?action=backup" }
  ],
  "commands": [
    {
      "id": "create-database",
      "label": "Create database",
      "accelerator": "Ctrl+Shift+D"
    }
  ],
  "event_subscriptions": [
    "instance.state_change",
    "job.completed"
  ],
  "dependencies": [
    "org.zaminmc.frontend.default@>=1.0.0"
  ],
  "signature": "..."
}
```

### 4.2 Permissions

`permissions` follows the same vocabulary as frontend packages
(see `docs/architecture/RUST_ENGINE.md` §3 PermissionGrant enum).
A database addon declaring `database.read`, `database.create`,
`database.update`, `database.delete`, `database.view_password`
can manage databases; it cannot start/stop a server (it lacks
`control.start` etc.).

The engine grants permissions on install (admin approval required
for elevated permissions). A permission cannot be elevated
silently; permission elevation requires admin re-approval.

### 4.3 Instance scope

`instance_scope: "required" | "optional" | "none"` declares whether
the addon's UI is per-instance (always shown under a specific
server), per-instance-optional (shown only for instances the addon
is enabled for), or global (not tied to an instance).

### 4.4 Page surfaces

Same as frontend packages: the explicit list of `zim://` URLs this
addon contributes. An addon's page surface cannot collide with the
default frontend's page surface or another addon's; collision
results in install failure.

### 4.5 Sidebar items

The default frontend's sidebar can include items contributed by
addons. Each item specifies a label, icon, and target `zim://`
page. The frontend renders these in a documented location (top of
sidebar for the active instance, bottom for global items).

### 4.6 Context menu items

Addons can contribute context-menu items on instance tabs (and on
items within instance pages, where supported). Each item specifies
a label and target page (with optional query string for action).

### 4.7 Commands

Addons can register commands with keyboard accelerators. These are
registered through the `BrowserWindowFeatures` extension point
(see `docs/architecture/BROWSER_INTEGRATION.md` §11). Commands
are scoped to the active ZIM surface.

### 4.8 Event subscriptions

Addons can subscribe to events from the engine. Each subscription
creates a WebSocket connection from the addon's page (or its
background worker, if any) to the engine. Event delivery follows
the same contract as the frontend's streaming (see
`docs/api/ZIM_API_SPEC.md` §11).

### 4.9 Dependencies

`dependencies` is a list of other addons or frontend packages
the addon requires. The engine resolves dependencies at install
time; an unsatisfiable dependency causes install failure (no
partial install).

### 4.10 Error isolation

An addon's page runs in its own renderer process (Chromium's
site-isolation handles this automatically because each `zim://`
URL has its own origin if the addon's `id` is part of the URL).
An addon that crashes its renderer does not affect other addons
or the default frontend.

If an addon's manifest becomes invalid (e.g. corrupted on disk),
the engine disables the addon on next load and surfaces a notice
in `zim://extensions`. Other addons and the default frontend are
unaffected.

### 4.11 Lifecycle

- Install: upload package via `POST /api/v1/extensions`; engine
  validates manifest, signature (if signed by trusted key, auto-install;
  otherwise admin approval), resolves dependencies, grants requested
  permissions (admin approval for elevated), extracts to data
  directory.
- Enable: `POST /api/v1/extensions/{id}/enable`.
- Disable: `POST /api/v1/extensions/{id}/disable`.
- Update: upload new version of an existing addon; engine migrates
  state (if any) and verifies downgrade-safety.
- Revoke: `DELETE /api/v1/extensions/{id}` removes the addon and
  revokes its permissions.

### 4.12 Native browser surfaces that are NOT replaceable

The prompt is explicit: *"Do not promise arbitrary replacement of
privileged native browser controls through CSS."* The following are
NOT replaceable by themes, frontend packages, or addons:

- The tab strip's drag/drop, pin, group behavior.
- The omnibox / address bar.
- The bookmark bar's behavior.
- The browser window's chrome (title bar, window controls).
- Keyboard shortcuts that are reserved by Chromium (e.g. Ctrl+N
  for new window, Ctrl+T for new tab, Ctrl+W for close tab).

A frontend package or addon can request *additional* keyboard
shortcuts via the `commands` manifest field, scoped to its own
active surface. It cannot override reserved shortcuts.

## 5. Default frontend

The default frontend (`zim/frontend/`) is the ZIM-authored default
frontend package. It is always bundled with the engine and cannot be
removed. It declares all standard page surfaces (newtab, servers,
console, files, settings, backups, schedules, subusers, metrics)
and uses the engine's default theme.

The default frontend is the *recovery frontend*: if a custom frontend
package fails its smoke test or is otherwise unusable, the engine
reverts to the default frontend and surfaces a recovery notice.

The default frontend's framework (whatever it is — the implementation
phase will choose; preliminary: a small vanilla-TS + Web Components
setup to minimize the engine's coupling to the framework) is NOT a
dependency of the engine. The engine loads the default frontend's
bytes via the WebUI handler; it does not import the frontend's
framework types or call framework-specific APIs.

## 6. Default extensions

ZIM ships a small set of bundled default extensions under
`zim/extensions/`:

- `org.zaminmc.extension.databases` — managed database UI.
- `org.zaminmc.extension.recycle-bin` — deleted-file recycle bin.
- `org.zaminmc.extension.audit-log` — audit log viewer.

These are installed by default on a fresh ZIM install and can be
disabled (but not removed) by the user. They use the same addon SDK
as third-party extensions; they have no special privileges.

## 7. What this specification does NOT cover

- The default frontend's exact component architecture (TBD in the
  implementation phase).
- The exact theme token set (preliminary: the set in §2.1; the
  final set is in `zim/contracts/theme-tokens.v1.json`).
- The exact accelerator set (preliminary: per-addon declared; no
  global registry beyond what Chromium provides).
- The packaging format for distributing extensions to end users
  (preliminary: signed `.zimx` archives; details in
  `docs/architecture/IMPLEMENTATION_ROADMAP.md`).

## 8. Workload integration packages (distinct from frontend packages)

A workload integration package is **not** a frontend package and
**not** a ZIM addon. It is a third category of package, governed by
`docs/architecture/WORKLOAD_ABSTRACTION.md` §5 and AD-15 / AD-16 in
`docs/architecture/DECISIONS.md`. The distinction is:

| Package type                | What it provides                                | Manifest                | Loaded by               |
|------------------------------|--------------------------------------------------|--------------------------|--------------------------|
| Theme                        | Token values only (no code).                    | `zim.theme.json`         | Frontend loader.         |
| Frontend package             | UI pages at `zim://` URLs.                      | `zim.frontend.json`     | Frontend loader.         |
| ZIM addon (extension)        | UI surfaces + commands + sidebar + events.     | `zim.addon.json`         | Engine extension loader. |
| **Workload integration**    | A `WorkloadKind` implementation (Rust library). | `zim.toml`               | Engine integration loader. |

The workload integration manifest format is canonical in
`docs/architecture/WORKLOAD_ABSTRACTION.md` §5; the engine's
loading mechanism is documented in
`docs/architecture/RUST_ENGINE.md` §12.4. The HTTP surface for
managing installed workload integrations is documented in
`docs/api/ZIM_API_SPEC.md` §10.16.

This specification does not duplicate the workload integration
contract; it cross-references it to keep the canonical location
singular.
