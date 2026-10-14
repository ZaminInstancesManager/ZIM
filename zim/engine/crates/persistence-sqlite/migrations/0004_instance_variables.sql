-- 0004: per-instance variable overrides (M10).
--
-- The migration importer's `server_variables` target and the
-- runtime's env-resolution override source. Keyed by
-- (instance_id, env_name) — deliberately NOT egg_variable_id:
-- `put_egg` replaces the egg's variable rows wholesale (fresh ids
-- per import, see store.rs), so an id-keyed override would be
-- cascade-deleted on every egg re-import; the env name is the
-- stable vocabulary that both the egg declarations and the
-- runtime environment share (per
-- docs/compatibility/MIGRATION_AND_COMPATIBILITY.md §2.3's
-- `server_variables` → `instance_variables` mapping).

CREATE TABLE instance_variables (
    instance_id  TEXT NOT NULL REFERENCES instances (id) ON DELETE CASCADE,
    env_name     TEXT NOT NULL,
    value        TEXT NOT NULL,
    PRIMARY KEY (instance_id, env_name)
);
