# Test Strategy (Phase 6)

> **CORRECTION (2026-10-15, AD-20):** Test layers referencing a
> built Chromium browser are superseded by the desktop-shell model:
> shell/UI tests run against the Tauri shell + the engine-served
> frontend (milestones M21+), not a patched Chromium. Engine test
> layers are unaffected.

This document specifies ZIM's test strategy: the test types, the
inventory of tests that protect each architectural invariant, the
fault-injection scenarios, the cross-platform test matrix, and the
verification discipline that prevents tests from being labeled as
passing when they have not been run.

It is canonical for ZIM's testing approach. The roadmap
(`docs/architecture/IMPLEMENTATION_ROADMAP.md`) records when each
test class reaches "written and passing" status.

## 1. Test types

ZIM uses five test types:

1. **Engine unit tests** (`#[test]` in Rust, `*_test.rs`).
   - Cover individual functions and small modules in isolation.
   - Run in milliseconds.
   - No I/O dependencies; mock any external service.
2. **Engine integration tests** (`tests/` directory in the Rust
   workspace).
   - Cover interactions between modules, including real persistence
   (in-memory SQLite), real runtime adapters (mock Docker or a test
   container), and real streaming.
   - Run in seconds.
   - May use Docker-in-Docker for runtime tests.
3. **Browser tests** (Chromium's `browser_tests` target).
   - Cover the Chromium-side integration layer: `zim://` URL
   routing, `ServerTabFeature` attachment, tab menu items, etc.
   - Run as part of the Chromium build.
   - Require a Chromium build environment (not in this workspace).
4. **End-to-end (E2E) tests** (a separate test runner that drives
   the full ZIM product).
   - Cover user-visible scenarios: install, create instance, start,
   console, files, backups, schedules, extensions, migration.
   - Run in seconds-to-minutes per test.
   - Require a built ZIM product (engine + browser) and Docker.
5. **Fault-injection tests** (a subset of integration and E2E
   tests).
   - Cover failure scenarios: engine crash, renderer crash, node
   disconnect, disk full, DB transaction failure, partial
   transfers, etc.
   - Use a documented fault-injection harness.

## 2. Invariant test inventory

Each lifecycle invariant from `docs/architecture/ZIM_ARCHITECTURE.md`
§4 has a test that verifies it. The inventory:

| Invariant # | Invariant description                                | Test type             | Test name                                              | Status |
|--------------|--------------------------------------------------------|-----------------------|--------------------------------------------------------|--------|
| 1            | Persisted `running` flag is not proof of liveness      | Engine integration    | `engine_integration::reconcile_after_restart`           | Delivered (M6) — application tests/supervision.rs + runtime-docker tests (reconcile) |
| 2            | Accepted start request is not proof that server started | Engine integration    | `engine_integration::start_job_state_machine`           | Delivered (M6) — supervision + api lifecycle tests (202 accepted) |
| 3            | Tab's existence is not proof that server should run     | E2E                    | `e2e::tab_does_not_imply_running`                       | Planned |
| 4            | Closing a tab does not stop its server                  | E2E                    | `e2e::close_tab_keeps_server_running`                  | Planned |
| 5            | Restoring a session does not issue lifecycle commands   | E2E                    | `e2e::session_restore_no_lifecycle`                    | Planned |
| 6            | Multiple tabs cannot create multiple server processes  | E2E                    | `e2e::three_tabs_one_process`                          | Planned |
| 7            | Lifecycle operations cannot race into undefined states  | Engine fault-injection | `engine_integration::power_action_race`                | Delivered (M6) — application tests/supervision.rs |
| 8            | Native process runner is not equivalent to container    | Engine integration    | `engine_integration::host_runtime_requires_consent`    | Planned |
| 9            | Per-instance operation serialization                    | Engine integration    | `engine_integration::power_queue_serializes`           | Delivered (M6) — application tests/supervision.rs |
| 10           | Engine-restart reconciliation                          | Engine integration    | `engine_integration::restart_reconciliation`           | Delivered (M6) — application + runtime-docker reconcile tests |
| 11           | Cleanup after partial failures                          | Engine fault-injection | `engine_integration::partial_failure_cleanup`           | Planned |
| 12           | Deletion and transfer coordination                      | Engine fault-injection | `engine_integration::delete_transfer_coordination`    | Planned |
| 13           | Stale event protection                                  | Engine fault-injection | `engine_integration::stale_event_protection`           | Delivered (M6, poll-form) — generation counter + discard guard |
| 14           | Crash-loop protection                                   | Engine integration    | `engine_integration::crash_loop_protection`           | Delivered (M6) — mock + real-Docker crash-loop tests |
| 15           | Capability checks at the engine boundary                | Engine integration    | `engine_integration::capability_enforcement`           | Planned |
| 16           | Restoration of default frontend on custom frontend failure | E2E                  | `e2e::custom_frontend_failure_recovers`                | Planned |

Each test name resolves to a real test file once written; the
"Status" column reflects the verification discipline (§7).

## 3. Failure-scenario test inventory

Per the prompt's Phase 12, the test strategy must cover these
failure scenarios. Each is a test:

| Scenario                                  | Test type                  | Test name                                                  | Status |
|--------------------------------------------|----------------------------|-------------------------------------------------------------|--------|
| Engine restart                              | Engine fault-injection     | `engine_integration::restart_reconciles_state`              | Delivered (M6) |
| Renderer crash                              | Browser test / E2E          | `e2e::renderer_crash_engine_unaffected`                     | Planned |
| Duplicate requests                          | Engine integration         | `engine_integration::idempotency_key_dedupes`               | Planned |
| Delayed process-exit events                 | Engine fault-injection     | `engine_integration::delayed_exit_event_ordering`           | Planned |
| Interrupted backups                         | Engine fault-injection     | `engine_integration::interrupted_backup_recovers`          | Planned |
| Failed transfers                            | Engine fault-injection     | `engine_integration::failed_transfer_rollback`              | Planned |
| Invalid extension packages                  | Engine integration         | `engine_integration::invalid_extension_rejected`            | Planned |
| Disk full                                   | Engine fault-injection     | `engine_integration::disk_full_resource_exhausted`         | Planned |
| DB transaction failure                      | Engine fault-injection     | `engine_integration::db_transaction_rollback`              | Planned |
| Node disconnect                             | Engine fault-injection     | `engine_integration::node_disconnect_marks_unknown`         | Planned |
| Custom frontend failure                     | E2E                         | `e2e::custom_frontend_failure_recovers`                     | Planned |
| Tab capability token theft attempt          | Engine integration         | `engine_integration::stolen_tab_token_rejected_after_close` | Planned |
| Path traversal attack                       | Engine integration         | `engine_integration::path_traversal_rejected`               | Planned |
| Archive extraction attack (zip bomb)        | Engine integration         | `engine_integration::zip_bomb_rejected`                     | Planned |
| DNS rebinding attack                        | Engine integration         | `engine_integration::dns_rebinding_blocked`                | Planned |
| Cross-origin request from web content       | Engine integration         | `engine_integration::cross_origin_blocked`                  | Planned |

## 4. Cross-platform test matrix

Where the same behavior must work across platforms, the test matrix
identifies the platforms each test runs on:

| Behavior                       | Linux x64 | Linux arm64 | Windows x64 | macOS arm64 | macOS x64 | Remote Linux node |
|--------------------------------|-----------|-------------|-------------|-------------|-----------|---------------------|
| Engine startup                 | Yes       | Yes         | Yes         | Yes         | Yes       | Yes                 |
| Container runtime (Docker)     | Yes       | Yes         | Yes (WSL2)  | Yes (VM)    | Yes (VM)  | Yes                 |
| Host runtime (opt-in)          | Yes       | Yes         | No          | No          | No        | Yes                 |
| URL protocol `zim://` registration | Yes   | Yes         | Yes         | Yes         | Yes       | (n/a)               |
| Auto-start on user login       | Yes (systemd) | Yes (systemd) | Yes (Service) | Yes (launchd) | Yes (launchd) | (n/a)               |
| Engine restart after crash     | Yes       | Yes         | Yes         | Yes         | Yes       | Yes                 |

Tests that are platform-specific are marked accordingly. Tests that
are platform-agnostic run on all platforms.

## 5. Upstream-protected tests

Tests from the upstream projects that ZIM runs as regression coverage
on each re-pin or local-patch change:

### Chromium upstream tests (relevant subset)

| Test target                                        | What it protects                       | Run condition |
|-----------------------------------------------------|-----------------------------------------|---------------|
| `tab_strip_model_unittests`                         | Tab model invariants                    | On each Chromium re-pin or tab-strip patch. |
| `session_restore_browsertest`                       | Session restore behavior                | On each re-pin or sessions-patch. |
| `chrome_serialized_navigation_driver_unittest`      | Navigation serialization                | On each re-pin. |
| `extensions_browsertests` (subset)                 | Extension system                       | On each re-pin or extensions-patch. |
| `browser_tests` (ZIM-touched subset)                | Browser features ZIM touches            | On each re-pin. |

### Pterodactyl Panel tests (reference only)

The Panel's PHPUnit tests are preserved in
`upstream/pterodactyl-panel/tests/` as a behavioral reference for
the Rust engine's integration tests. They are not run directly (no
PHP runtime in the workspace; the engine's own integration tests
reproduce the behavior in Rust).

### Pterodactyl Wings tests (reference only)

The Wings `*_test.go` files are preserved as a behavioral reference.
Not run directly (no Go toolchain in the workspace); the engine's
own integration tests reproduce the behavior in Rust.

## 6. Test environment requirements

The full test suite requires:

- Rust toolchain (`rustc`, `cargo`) — not in current workspace.
- Docker Engine (for container runtime tests).
- A Chromium build environment (`depot_tools`, `gn`, `ninja`,
  sufficient disk/memory) — not in current workspace.
- A Linux test environment for fault-injection (some scenarios are
  Linux-specific; documented in §4).

The current workspace cannot run any of these tests; they are
planned, not executed. This is recorded in
`docs/audits/UPSTREAM_AUDIT.md` §verification status.

## 7. Verification discipline

The prompt is explicit: *"Never label an unexecuted test as
passing."* The verification categories used throughout ZIM's docs
are:

- **Verified:** The test was run in this environment and passed.
  (Used for: file-presence checks via `ls`; directory enumerations
  via the GitHub contents API; spot-fetched file contents; git log
  inspection.)
- **Partially verified:** The test was run but only partially
  (e.g. only the head of a file was read; the rest is unverified).
- **Not verified:** The test is identified but not run (e.g. an
  engine unit test that requires a Rust toolchain).
- **Blocked by environment:** The test cannot be run in the current
  environment (e.g. a Chromium build test in a 9 GB workspace).
- **Deferred to implementation:** The test is identified for the
  implementation phase; it will be written and run then.

The "Status" column in the inventories above uses only these
categories. As of the end of the architecture phase:

- All invariant tests: `Planned` (Not verified).
- All failure-scenario tests: `Planned` (Not verified).
- All upstream-protected tests: `Blocked by environment`.
- All E2E tests: `Planned` (Not verified).

This is honest: no test has been run; the architecture phase
identifies *which tests must exist and pass before any capability
is claimed as "Verified"*.

## 8. Test-runner CI integration

When the implementation phase reaches the point of running tests,
ZIM's CI runs:

- On every push: engine unit tests + engine integration tests
  (fast).
- On every merge to main: + E2E tests + fault-injection tests
  (slower).
- On every Chromium re-pin: + upstream-protected tests.
- Nightly: full cross-platform test matrix (where runners are
  available for each platform).

CI configuration is in `.github/workflows/` (TBD in implementation).

## 9. Test data and fixtures

- Migration E2E tests use a fixture Pterodactyl Panel + Wings
  Docker Compose deployment (in `tests/fixtures/pterodactyl/`).
- Egg compatibility tests use a curated set of fixture eggs (in
  `tests/fixtures/eggs/`), one per parser variant.
- Fault-injection tests use a fault-injection harness
  (`tests/fault_injection/`) that can inject: container exits,
  network partitions, disk-full simulation, DB transaction
  failures, etc.

## 11. Coverage discipline

ZIM does not enforce a coverage percentage target; instead, it
enforces a *behavior coverage* target: every architectural invariant
in `docs/architecture/ZIM_ARCHITECTURE.md` §4 must have a
corresponding test in the inventory in §2 above. Coverage tools
(`tarpaulin` for Rust, `lcov` for Chromium C++) are run in CI and
the results are reported, but the gate is the inventory, not a
percentage.

## 12. Expanded-scope invariant tests (workload abstraction)

The expanded scope (see `docs/architecture/WORKLOAD_ABSTRACTION.md`)
introduces additional invariants beyond the original 16. Each has a
test in the inventory below; verification discipline (§7) applies
— all are `Planned` until the corresponding implementation milestone
produces a passing test.

| # | Invariant                                                                                  | Test type                  | Test name                                                | Status |
|---|----------------------------------------------------------------------------------------------|----------------------------|----------------------------------------------------------|--------|
| E1 | A workload's required capabilities must be a subset of its runtime's declared capabilities.   | Engine integration         | `engine_integration::placement_capability_check`         | Planned |
| E2 | A workload placed on an incompatible runtime is rejected at creation time.                  | Engine integration         | `engine_integration::placement_rejects_incompatible`     | Planned |
| E3 | A capability call on a runtime that does not implement the trait returns `Unsupported`.     | Engine integration         | `engine_integration::unsupported_returns_error`         | Planned |
| E4 | A workload's tab is a view; closing the tab does not stop the workload (generalized invariant 4 for all kinds). | E2E                        | `e2e::close_tab_keeps_workload_running`                  | Planned |
| E5 | Multiple tabs of the same workload are multiple views, not multiple workloads (generalized invariant 6). | E2E                        | `e2e::three_tabs_one_workload`                           | Planned |
| E6 | Restoring a session does not issue workload lifecycle commands (generalized invariant 5).  | E2E                        | `e2e::session_restore_no_lifecycle_workload`            | Planned |
| E7 | A workload in `Recovering` state is shown yellow with a "recovering" tooltip, not green.    | E2E                        | `e2e::recovering_state_visual_mapping`                  | Planned |
| E8 | A workload in `CrashLoop` state is shown red with a glyph, not solid red.                    | E2E                        | `e2e::crash_loop_visual_mapping`                        | Planned |
| E9 | A workload on a disconnected remote node is shown gray with an "unknown" tooltip, not red. | E2E                        | `e2e::disconnected_node_visual_mapping`                 | Planned |
| E10 | A workload integration's `WorkloadKind` scope is enforced at the API boundary.           | Engine integration         | `engine_integration::integration_kind_scope_enforced`    | Planned |
| E11 | A `Migratable::migration_plan` for a database returns `requires_quiesce: true`.           | Engine integration         | `engine_integration::database_migration_quiesces`       | Planned |
| E12 | An interrupted migration can be rolled back to a `MigrationCheckpoint`.                    | Engine fault-injection     | `engine_integration::migration_rollback`                | Planned |
| E13 | A workload tab remains meaningful when its workload is stopped (shows the stopped state, not an error). | E2E                        | `e2e::stopped_workload_tab_meaningful`                  | Planned |
| E14 | A workload tab remains meaningful when its workload is on an unreachable remote node.      | E2E                        | `e2e::unreachable_node_tab_meaningful`                  | Planned |
| E15 | The legacy `/api/v1/instances` surface produces the same responses as `/api/v1/workloads` for `GameServer` workloads. | Engine integration         | `engine_integration::legacy_instances_alias`            | Planned |
| E16 | The `/api/v1/workloads/{id}/console/input` endpoint returns `409 unsupported` for a non-`ConsoleCapable` workload. | Engine integration         | `engine_integration::console_unsupported_for_website`   | Planned |
