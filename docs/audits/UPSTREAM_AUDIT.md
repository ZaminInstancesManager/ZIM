# Upstream Audit — Source Trees, Revisions, Verification

> **CORRECTION (2026-10-15, AD-20):** Chromium-fork references herein
> are superseded — ZIM is a Chrome-inspired desktop application, not a
> browser. Retained as the historical record of the superseded plan;
> see `docs/architecture/DECISIONS.md` AD-20.

This is the canonical record of what upstream source is present in the ZIM
repository, what upstream source is referenced but not present, the exact
revisions, the import methods, and what was actually verified. Other audit
and architecture documents reference this file rather than re-stating the
pins.

It satisfies Phase 1 of the assignment (establish the upstream forks) and
provides the evidence base for the Phase 2 and Phase 3 audits.

## Upstream pin table

| Project            | Canonical upstream URL                              | Pinned revision | Full commit SHA                                  | Retrieval date (UTC+08) | Import method                            | License     | Source present in repo | Build-verified |
|--------------------|------------------------------------------------------|------------------|----------------------------------------------------|--------------------------|------------------------------------------|-------------|------------------------|----------------|
| Pterodactyl Panel  | https://github.com/pterodactyl/panel                 | `v1.15.1`        | `e98678c5a1d07a74036660a459d87a4eb7c105c1`        | 2026-10-13               | `git subtree add` (history-preserving)   | MIT         | Yes                    | No             |
| Pterodactyl Wings  | https://github.com/pterodactyl/wings                  | `v1.13.3`        | `6987d5e6f0612133e031255a99655e822d30579a`       | 2026-10-13               | `git subtree add` (history-preserving)   | MIT         | Yes                    | No             |
| Chromium           | https://chromium.googlesource.com/chromium/src.git   | `157.0.8103.0`   | `b0a05135bf5a60bb3ab5a1e86a0e55193cb126d0`       | 2026-10-13               | Deferred (manifest-only at this phase)   | BSD-style   | No                     | No             |

The full SHA for each Pterodactyl project is the SHA of the tagged release
commit as reported by `git log -1 v1.15.1` (resp. `v1.13.3`) after the
subtree fetch. The Chromium SHA is the value of the
`/repos/chromium/chromium/git/refs/tags/157.0.8103.0` GitHub API ref object.

## Pterodactyl Panel — verification

### Import method

```
git -C /home/z/zim-work subtree add \
  --prefix=upstream/pterodactyl-panel \
  https://github.com/pterodactyl/panel.git \
  v1.15.1
```

The `git subtree add` command performs a subtree merge: it fetches the named
remote's history, then creates a merge commit on the current branch whose
tree contains the remote's tree at the named revision under the given prefix.
History is preserved (no `--squash`); every upstream commit reachable from
`v1.15.1` is now reachable from ZIM's `main` via the merge commit's second
parent.

The merge commit on ZIM `main`:

```
2359256de Add 'upstream/pterodactyl-panel/' from commit 'e98678c5a1d07a74036660a459d87a4eb7c105c1'
```

### Verification performed

- **Source tree present at expected path.** Verified by listing
  `upstream/pterodactyl-panel/` and confirming expected top-level files
  (`README.md`, `composer.json`, `artisan`, `app/`, `database/`, `routes/`,
  `config/`, etc.).
- **Pinned revision reachable in history.** Verified by `git log -1
  v1.15.1` in the local clone; output identifies the commit as
  `e98678c5a1d07a74036660a459d87a4eb7c105c1` with subject "Notify the CDN
  manifest repo when a release is tagged".
- **Upstream history preserved.** Verified by `git log --oneline` showing
  historical Panel commits (e.g. `1d791ad6 upgrade to laravel 12.x`,
  `73d2550e relax overly strict permission guards on running schedules`)
  reachable from ZIM's `main` through the subtree merge commit's ancestry.
- **File count.** `git ls-files | wc -l` reports 1665 files in the working
  tree (combined across ZIM-original content, Panel, and Wings subtrees).
- **Disk impact.** `.git` size after both Pterodactyl imports: ~30 MB.
  `upstream/` working tree size: ~16 MB.

### Not verified

- **Composer dependency install.** `composer install` was not run (no PHP
  runtime in workspace). `composer.lock` is present and is the upstream-pinned
  lockfile at `v1.15.1`.
- **Laravel test suite.** `php artisan test` (or `phpunit`) was not run (no
  PHP runtime). The Panel's tests are preserved in the source tree and
  identified for later reference in
  `docs/audits/PTERODACTYL_ARCHITECTURE.md`.
- **Frontend build.** `npm run build` was not run. The Panel's frontend
  tooling (Vite, Vue, Tailwind) is identified but not exercised.

### Future upstream sync for Panel

```
git -C /home/z/zim-work subtree pull \
  --prefix=upstream/pterodactyl-panel \
  https://github.com/pterodactyl/panel.git \
  <new-tag>
```

Conflicts will arise only if ZIM has modified files under
`upstream/pterodactyl-panel/`. The disposition in
`docs/audits/COMPONENT_DISPOSITION.md` treats `upstream/pterodactyl-panel/`
as a frozen reference, so ZIM will not modify files under that path; subtree
pulls should apply cleanly.

## Pterodactyl Wings — verification

### Import method

```
git -C /home/z/zim-work subtree add \
  --prefix=upstream/pterodactyl-wings \
  https://github.com/pterodactyl/wings.git \
  v1.13.3
```

The merge commit on ZIM `main`:

```
4074be838 Add 'upstream/pterodactyl-wings/' from commit '6987d5e6f0612133e031255a99655e822d30579a'
```

### Verification performed

- **Source tree present at expected path.** Verified by listing
  `upstream/pterodactyl-wings/` and confirming expected top-level files
  (`wings.go`, `go.mod`, `cmd/`, `config/`, `environment/`, `events/`,
  `internal/`, `router/`, `server/`, `sftp/`, `system/`).
- **Pinned revision reachable in history.** Verified by `git log -1
  v1.13.3`; the commit subject is "container CPU shares configurable".
- **Module declaration.** `go.mod` declares
  `module github.com/pterodactyl/wings`, `go 1.24.0`, toolchain `go1.24.1`.
  Module path and Go version recorded for future cross-checks.
- **Dependency inventory.** Top-level `require` block in `go.mod` includes
  `github.com/docker/docker v28.3.3+incompatible` (container runtime client),
  `github.com/docker/go-connections`, `github.com/gin-gonic/gin v1.10.1`
  (HTTP framework), `github.com/gorilla/websocket v1.5.3` (console WS),
  `github.com/glebarez/sqlite v1.11.0` (local state DB),
  `github.com/go-co-op/gocron v1.37.0` (schedules),
  `github.com/gbrlsnchs/jwt/v3 v3.0.1` (Panel↔Wings JWT auth),
  `github.com/beevik/etree v1.5.0` (XML config parsing),
  `github.com/gammazero/workerpool v1.1.3` (per-server power-action
  serialization), and others. Full list in `go.mod`.

### Not verified

- **Go module download.** `go mod download` was not run (no Go toolchain in
  workspace).
- **Wings build.** `go build ./...` was not run (no Go toolchain).
- **Wings test suite.** `go test ./...` was not run (no Go toolchain). The
  Wings test files (`*_test.go`) are present in the source tree and are
  identified for later reference.

### Future upstream sync for Wings

Same `git subtree pull` pattern as Panel. The same disposition rule
(`upstream/pterodactyl-wings/` is a frozen reference) applies.

## Chromium — verification

### Import method (current phase)

No source is imported. The pin is recorded as a manifest in
`chromium/UPSTREAM_MANIFEST.md`. Source inspection during the audit phase is
performed by direct HTTPS fetches against the chromium/chromium GitHub mirror
at the pinned revision (raw file fetches via `raw.githubusercontent.com`
and directory enumeration via the GitHub contents API). Fetched files are
kept locally under `/home/z/chromium-inspect/` as inspection evidence; they
are **not** part of the ZIM repository.

### Import method (planned, first implementation milestone)

```
git -C /home/z/zim-work subtree add \
  --prefix=chromium/src \
  https://chromium.googlesource.com/chromium/src.git \
  157.0.8103.0
```

followed by `gclient sync` against the pinned revision to populate the
`DEPS` dependencies. This is **blocked by environment** in the current
workspace (insufficient disk, no `depot_tools`); see
`chromium/UPSTREAM_MANIFEST.md` for the full constraint list.

### Verification performed (current phase)

- **Tag exists on chromium/chromium GitHub mirror.** Verified by GitHub API
  call `GET /repos/chromium/chromium/git/refs/tags/157.0.8103.0`; returned
  object SHA `b0a05135bf5a60bb3ab5a1e86a0e55193cb126d0`.
- **Top-level source tree enumerated.** Verified by `GET
  /repos/chromium/chromium/contents?ref=157.0.8103.0`; returned all expected
  top-level entries (`.gn`, `BUILD.gn`, `DEPS`, `LICENSE`, `README.md`,
  `AUTHORS`, `OWNERS`, `PRESUBMIT.py`, `chrome/`, `content/`, `components/`,
  `extensions/`, `base/`, `net/`, `mojo/`, `services/`, `third_party/`,
  `tools/`, `ui/`, `v8`, etc.).
- **`chrome/browser/` enumerated.** Verified by contents API call against
  `chrome/browser?ref=157.0.8103.0`; returned 594 entries, including
  `BUILD.gn`, `DEPS`, `PRESUBMIT.py`, `README.md`, and subdirectories
  `sessions/`, `ui/`, `extensions/`, `about_flags.cc`, etc.
- **`chrome/browser/ui/tabs/` enumerated.** Verified; returned the full tab
  management source set including `tab_strip_model.{h,cc}`,
  `tab_strip_model_observer.{h,cc}`, `tab_model.{h,cc}`, `tab_data.{h,cc}`,
  `tab_features.{h,cc}`, `tab_menu_model.{h,cc}`, `tab_group_model.{h,cc}`,
  `pinned_tab_service.{h,cc}`, `recent_tabs_builder.{h,cc}`, plus an
  extensive split-view implementation (`split_tab_util.{h,cc}`,
  `split_tab_highlight_controller.{h,cc}`,
  `split_tab_menu_model.{h,cc}`,
  `split_tab_metrics.{h,cc}`,
  `split_view_layout_menu_model.{h,cc}`,
  `split_view_iph_controller.{h,cc}`).
- **`chrome/browser/sessions/` enumerated.** Verified; returned the full
  session restore infrastructure including `session_restore.{h,cc}`,
  `session_restore_delegate.{h,cc}`, `session_service.{h,cc}`,
  `session_service_log.{h,cc}` (append-only session change log),
  `session_data_service.{h,cc}`,
  `session_data_deleter.{h,cc}`,
  `exit_type_service.{h,cc}` (normal vs crash exit),
  `tab_restore_service_*.{h,cc}`,
  `restore_on_startup_policy_handler.{h,cc}`.
- **`content/public/browser/` enumerated.** Verified; returned 695+ entries
  (the public content API for embedders), including `web_contents.h`,
  `navigation_controller.h`, `browser_thread.h`, `render_view_host.h`, and
  the rest of the embedding surface used by `chrome/`.
- **Key file contents spot-fetched.** Spot-fetched via raw HTTPS:
  `chrome/browser/ui/tabs/tab_strip_model.h` (1551 lines),
  `chrome/browser/ui/browser.h` (303 lines),
  `chrome/browser/ui/tabs/split_tab_util.h` (33 lines),
  `chrome/common/url_constants.h` (749 lines — `chrome://` URL constants),
  `DEPS` (4978 lines), `.gn` (479 lines).
- **`Browser` class inheritance and ownership.** Verified from
  `chrome/browser/ui/browser.h`: `class Browser : public
  TabStripModelObserver, public WebContentsCollection::Observer, public
  BrowserWindowInterface`. Doc-comment: "An instance of this class
  represents a single browser window on Desktop. Owned by
  BrowserManagerService."
- **`split_tabs` namespace.** Verified from `tab_strip_model.h`: the file
  declares forward types under `namespace split_tabs { class SplitTabData;
  class SplitTabVisualData; enum class SplitTabLayout; enum class
  SplitTabCreatedSource; enum class SplitTabOrientationChangeSource; }` —
  split-view is a real, first-class Chromium 157 feature with layout,
  orientation and source enums.
- **License file present.** Verified; `LICENSE` is present at the pinned
  revision and is the BSD-style Chromium license.

### Not verified (current phase)

- **Chromium source tree in ZIM repository.** Not present; deferred per
  import strategy.
- **`DEPS` dependencies fetched.** Not fetched; no `depot_tools` in
  workspace.
- **`gn gen` build configure.** Not performed; no GN toolchain.
- **`ninja` build.** Not performed; insufficient disk for build outputs.
- **Chromium test suite.** Not run; no build.

## License and notice preservation

- `upstream/pterodactyl-panel/LICENSE.md` — Pterodactyl Panel MIT license,
  preserved verbatim in the subtree import.
- `upstream/pterodactyl-wings/LICENSE` — Pterodactyl Wings MIT license,
  preserved verbatim in the subtree import.
- `LICENSE` (ZIM root) — Apache 2.0, the existing user-authored ZIM
  license; not modified.
- Chromium `LICENSE` — verified present at the pinned revision upstream;
  will be preserved verbatim when the source tree is imported.

## Summary of verification categories

Per the prompt's classification scheme:

- **Verified:** Pterodactyl Panel source present, pinned revision reachable,
  upstream history preserved, license file present, ZIM working tree
  consistent. Same for Pterodactyl Wings. Chromium pinned tag exists,
  top-level tree enumerated, key subsystem paths enumerated, key file
  contents spot-fetched, license file present at upstream.
- **Partially verified:** (none — each item above is either fully verified
  or not verified).
- **Not verified:** Composer/PHP build, Wings Go build, Chromium `gclient
  sync`, Chromium `gn gen`, Chromium `ninja` build, all upstream test
  suites (no toolchains in workspace).
- **Blocked by environment:** All Chromium build-related verifications are
  blocked by the absence of `depot_tools`, insufficient disk, and
  insufficient memory. Rust engine build verification is blocked by the
  absence of a Rust toolchain.
- **Deferred to implementation:** Running upstream test suites, building
  Chromium, building the Rust engine, running the browser integration
  smoke tests.
