-- 0003: the egg store (M9).
--
-- Eggs (server definitions) and their declared variables. The
-- satellite payloads (images, features, scripts, config parser
-- rules) are preserved verbatim in JSON columns — they are the
-- egg author's data, consumed whole by the runtime; only the
-- variables get a relational table (they are individually
-- addressed: per-instance overrides, user-editable flags).

CREATE TABLE eggs (
    id          TEXT PRIMARY KEY,
    legacy_uuid TEXT UNIQUE,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    author      TEXT NOT NULL DEFAULT '',
    images      TEXT NOT NULL DEFAULT '[]',
    startup     TEXT NOT NULL,
    features    TEXT NOT NULL DEFAULT '{}',
    scripts     TEXT NOT NULL DEFAULT '{}',
    config      TEXT NOT NULL DEFAULT '{}',
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE egg_variables (
    id            TEXT PRIMARY KEY,
    egg_id        TEXT NOT NULL REFERENCES eggs(id) ON DELETE CASCADE,
    env_name      TEXT NOT NULL,
    name          TEXT NOT NULL,
    description   TEXT NOT NULL DEFAULT '',
    default_value TEXT NOT NULL DEFAULT '',
    user_viewable INTEGER NOT NULL DEFAULT 1,
    user_editable INTEGER NOT NULL DEFAULT 0,
    rules         TEXT NOT NULL DEFAULT '[]',
    UNIQUE (egg_id, env_name)
);

CREATE INDEX egg_variables_by_egg ON egg_variables(egg_id);
