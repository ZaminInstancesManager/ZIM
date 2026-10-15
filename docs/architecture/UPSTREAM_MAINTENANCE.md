# Upstream Maintenance (Phase 6)

> **CORRECTION (2026-10-15, AD-20):** Section 3 (Chromium
> maintenance) is **cancelled** — no Chromium source is imported or
> patched. The patch queue / registry / resolution-log subsections
> are void, retained only as the historical record of the superseded
> plan. Pterodactyl maintenance (§2) is unaffected.

This document specifies how ZIM maintains its forks of Chromium and
Pterodactyl over time: how patches are carried, how upstream
revisions are updated, how conflicts are resolved, and how
compatibility is validated after each change.

It is canonical for ZIM's upstream-maintenance strategy.

## 1. Source trees and their maintenance model

| Source tree                    | Import method                 | Maintenance model |
|--------------------------------|-------------------------------|-------------------|
| `upstream/pterodactyl-panel/`  | git subtree (history-preserving) | Frozen reference. ZIM does not modify files under this path. Updates pull from upstream `pterodactyl/panel` on a deliberate cadence. |
| `upstream/pterodactyl-wings/`  | git subtree (history-preserving) | Frozen reference. Same as above for `pterodactyl/wings`. |
| `chromium/src/` (TBD)         | git subtree (deferred)         | Live fork. ZIM carries local patches under `chromium/PATCHES/`. Updates pull from upstream `chromium/src.git` on a deliberate cadence. |

## 2. Pterodactyl maintenance

### 2.1 Update cadence

- **Pterodactyl Panel:** re-pin to the latest stable Pterodactyl
  Panel release on a quarterly cadence (or sooner if a security
  advisory is issued).
- **Pterodactyl Wings:** same cadence.

### 2.2 Update process

```
# For Panel:
git -C /home/z/zim-work subtree pull \
  --prefix=upstream/pterodactyl-panel \
  https://github.com/pterodactyl/panel.git \
  v1.15.2

# For Wings:
git -C /home/z/zim-work subtree pull \
  --prefix=upstream/pterodactyl-wings \
  https://github.com/pterodactyl/wings.git \
  v1.13.4
```

Because ZIM does not modify files under `upstream/pterodactyl-*`,
the subtree pull should apply cleanly with no conflicts. If a
conflict arises, it indicates that someone has modified an
upstream file (which violates the disposition in
`docs/audits/COMPONENT_DISPOSITION.md`); the conflict must be
resolved by reverting the modification and re-doing the
disposition audit for the modified file.

### 2.3 Post-update verification

After a Pterodactyl re-pin:

1. Re-run the Pterodactyl architecture audit
   (`docs/audits/PTERODACTYL_ARCHITECTURE.md`) — confirm the
   cited source files still exist at the new revision; update
   line counts and any excerpts that have changed.
2. Re-run the component-disposition matrix
   (`docs/audits/COMPONENT_DISPOSITION.md`) — confirm the
   dispositions still hold; update entries for components that
   have been added, removed, or substantially changed upstream.
3. Re-run the feature-parity matrix
   (`docs/audits/FEATURE_PARITY_MATRIX.md`) — same.
4. Update `docs/audits/UPSTREAM_AUDIT.md` §Pterodactyl pin table
   with the new revision, retrieval date, and verification status.

### 2.4 When to NOT re-pin

ZIM does not re-pin if:

- The new Pterodactyl release introduces a breaking change to the
  legacy client API surface that ZIM preserves (deferred
  compatibility) — wait for ZIM to update its compatibility
  surface first.
- The new release requires a PHP or Go version not available in
  the test environment.

In these cases, the re-pin is deferred and the
limitation is recorded in `docs/audits/UPSTREAM_AUDIT.md`.

## 3. Chromium maintenance

### 3.1 Update cadence

- **Chromium stable channel:** re-pin to a stable-channel release
  on a 4-week cadence (matching Chromium's stable release cadence)
  once the source tree is imported.
- **Security advisories:** re-pin immediately if a Chromium
  security advisory affects ZIM-touched surfaces.

### 3.2 Patch queue

ZIM's local Chromium patches live under `chromium/PATCHES/`:

```
chromium/PATCHES/
├── series                          # ordered list of patches
├── 0001-zim-scheme.patch
├── 0002-webui-controller-factory.patch
├── 0003-server-tab-feature.patch
├── 0004-server-tab-style.patch
├── 0005-tab-menu-model.patch
├── 0006-crash-keys.patch
└── 0007-feature-disables.patch    # optional; disabled features
```

Each patch file is a single logical change. Each patch has a
corresponding entry in this document under §3.4 with:

- Patch number and short name.
- Purpose (one paragraph).
- Source evidence justifying the change (referencing the audit
  documents).
- The upstream revision the patch was first applied against.
- The regression test that protects the patch (referencing
  `docs/architecture/TEST_STRATEGY.md`).
- The expected breakage if the patch fails to apply (so the
  resolver knows what to test).

### 3.3 Update process

```
# 1. Update the upstream pin in chromium/UPSTREAM_MANIFEST.md.
# 2. Pull the new revision:
git -C /home/z/zim-work subtree pull \
  --prefix=chromium/src \
  https://chromium.googlesource.com/chromium/src.git \
  <new-tag>

# 3. Resolve conflicts in the local patch queue:
#    For each patch in chromium/PATCHES/series, in order:
#      - Apply the patch.
#      - If it applies cleanly, continue.
#      - If it conflicts, resolve the conflict, update the patch
#        file, and record the resolution in this document under
#        §3.5 (patch resolution log).
# 4. Run gclient sync to refresh dependencies.
# 5. Run gn gen out/Default && ninja -C out/Default chrome.
# 6. Run the upstream-protected tests identified in
#    docs/architecture/TEST_STRATEGY.md §5.
# 7. Run the ZIM E2E tests.
```

### 3.4 Patch registry (initial entries)

#### Patch 0001: zim:// scheme registration

- **Purpose:** Register the `zim://` URL scheme and its constants
  in `chrome/common/url_constants.h` and related files.
- **Source evidence:** `chrome/common/url_constants.h` enumerated
  at the pinned Chromium revision (749 lines); the
  `WebUIControllerFactory` mechanism enumerated under
  `chrome/browser/ui/webui/`.
- **First applied against:** Chromium `157.0.8103.0` (commit
  `b0a05135bf5a60bb3ab5a1e86a0e55193cb126d0`).
- **Regression test:** `browser_tests/ZimWebUI` (browser test that
  navigates to `zim://newtab` and verifies the page loads).
- **Expected breakage if patch fails:** The ZIM NTP fails to load;
  `zim://` URLs return 404; the browser cannot reach ZIM pages.

#### Patch 0002: WebUIControllerFactory extension

- **Purpose:** Register the `WebUIControllerFactory` that maps
  `zim://` hosts to ZIM's `WebUIController` subclasses.
- **Source evidence:** `chrome/browser/ui/webui/` enumerated at
  the pinned revision; the factory registration mechanism is the
  standard Chromium WebUI extension point.
- **First applied against:** Chromium `157.0.8103.0`.
- **Regression test:** `browser_tests/ZimWebUI`.
- **Expected breakage if patch fails:** Same as patch 0001.

#### Patch 0003: ServerTabFeature attachment

- **Purpose:** Add `ServerTabFeature` to the `TabFeatures` graph.
- **Source evidence:** `chrome/browser/ui/tabs/tab_features.h`
  present at the pinned revision (full API is INVESTIGATE — see
  `docs/audits/COMPONENT_DISPOSITION.md` §5).
- **First applied against:** Chromium `157.0.8103.0`.
- **Regression test:** `browser_tests/ServerTabFeature`.
- **Expected breakage if patch fails:** Per-tab server status
  indicators do not appear; server-tab context menu items do not
  appear.

#### Patch 0004: ServerTabStyle decorator

- **Purpose:** Extend `TabStyle` with a `ServerTabStyle` decorator
  that renders the colored status indicator.
- **Source evidence:** `chrome/browser/ui/tabs/tab_style.{h,cc}`
  present at the pinned revision.
- **First applied against:** Chromium `157.0.8103.0`.
- **Regression test:** `browser_tests/ServerTabStyle`.
- **Expected breakage if patch fails:** Status indicators do not
  render; tabs look like ordinary Chromium tabs.

#### Patch 0005: TabMenuModel ZIM items

- **Purpose:** Add ZIM server-tab items (Start/Stop/Restart/Open in
  split view/Copy URL) to the `TabMenuModel`.
- **Source evidence:** `chrome/browser/ui/tabs/tab_menu_model.{h,cc}`
  present at the pinned revision.
- **First applied against:** Chromium `157.0.8103.0`.
- **Regression test:** `browser_tests/ServerTabMenu`.
- **Expected breakage if patch fails:** ZIM menu items do not
  appear in the tab context menu.

#### Patch 0006: ZIM crash keys

- **Purpose:** Register ZIM-specific crash keys (active frontend
  package version, engine version, whether any tab is a server
  tab).
- **Source evidence:** Chromium's crash-key registration mechanism
  (`components/crash/`) present at the pinned revision.
- **First applied against:** Chromium `157.0.8103.0`.
- **Regression test:** (none; crash keys are diagnostic).
- **Expected breakage if patch fails:** ZIM-specific crash context
  is missing from crash dumps.

#### Patch 0007 (optional): Feature disables

- **Purpose:** Disable specific Chromium features not wanted in
  ZIM (candidates identified during implementation; see
  `docs/audits/REMOVALS.md` candidate R5).
- **Source evidence:** TBD during implementation.
- **First applied against:** (TBD).
- **Regression test:** (TBD per feature).
- **Expected breakage if patch fails:** The disabled features
  re-appear; not a correctness issue, but a product-identity one.

### 3.5 Patch resolution log

Each time a patch is re-applied against a new upstream revision
and a conflict is resolved, an entry is added here:

```
yyyy-mm-dd: Patch NNNN conflicted against <new-tag>.
  Conflict: <description of the conflict>.
  Resolution: <description of the resolution>.
  Test: <which test verified the resolution>.
```

The log starts empty; entries are added as re-pins happen.

### 3.6 Patch backport policy

ZIM does not backport patches from upstream to older ZIM
revisions. The maintenance model is "always forward": a ZIM
release is pinned to one Chromium revision and one set of local
patches. Users who need a security fix update to the latest ZIM
release, which carries the latest Chromium stable + the latest
local patch queue.

This is the same model Chromium itself uses for its stable channel
(no backports to older stable revisions).

## 4. Conflict resolution principles

When a subtree pull or a patch re-application produces conflicts:

1. **Preserve upstream behavior unless the local patch is
   explicitly documented.** A conflict is resolved in favor of
   the local patch only where the patch's entry in §3.4 explicitly
   requires it.
2. **Record the resolution.** Every conflict resolution goes into
   §3.5 (for Chromium) or §2.2 (for Pterodactyl, where a conflict
   indicates a process violation).
3. **Re-run the relevant tests.** A conflict resolution that
   affects a test identified in §3.4's regression test must re-run
   that test before merge.
4. **Update the audit documents if the conflict changes the
   cited source evidence.** E.g. if a patch re-application moves
   the cited file's line count from 1551 to 1602, the audit
   document's line count is updated.

## 5. Verification after maintenance

After any maintenance operation (re-pin or patch re-application),
the following are verified:

- The build still succeeds (where the build environment supports
  it).
- The upstream-protected tests still pass (where the test
  environment supports it).
- The ZIM E2E tests still pass.
- The audit documents' cited source evidence still matches the
  repository (line counts, file presence, source excerpts).

The verification is recorded in the commit message of the
maintenance commit.

## 6. Maintenance cadence summary

| Maintenance operation                         | Cadence                  | Trigger conditions |
|------------------------------------------------|--------------------------|--------------------|
| Pterodactyl Panel re-pin                      | Quarterly                | New stable release; or security advisory. |
| Pterodactyl Wings re-pin                      | Quarterly                | Same. |
| Chromium stable re-pin                        | 4-week (matches Chromium stable cadence) | New Chromium stable; or security advisory. |
| Local patch queue review                       | 4-week (after Chromium re-pin) | Conflicts resolved; patch queue reviewed for relevance. |
| Full audit re-verification                     | 6-month                  | All audit documents' source-evidence citations re-verified against current upstream. |

## 7. Maintenance tooling

The maintenance operations are scripted under `tools/maintenance/`:

- `tools/maintenance/repin-pterodactyl.sh` — re-pin Panel or Wings.
- `tools/maintenance/repin-chromium.sh` — re-pin Chromium (after
  source import).
- `tools/maintenance/apply-patches.sh` — apply the local Chromium
  patch queue.
- `tools/maintenance/verify-audits.sh` — re-verify audit document
  citations.

These scripts are TBD in the implementation phase; the maintenance
process is documented here so the scripts have a clear contract.
