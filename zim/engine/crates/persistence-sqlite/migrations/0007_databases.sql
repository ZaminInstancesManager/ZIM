-- M31 managed databases: the panel-side rows — which database
-- hosts exist, and which databases are provisioned for which
-- instances.
--
-- The shape mirrors Pterodactyl's `database_hosts` and `databases`
-- tables verbatim (verified against
-- upstream/pterodactyl-panel/database/migrations/
-- 2016_02_07_181319_add_database_servers_table.php, the
-- 2017_03_16 rename to `database_hosts`, 2016_02_07_172148's
-- `databases`, and 2020_04_22's `max_connections` column), because
-- those rows are exactly what a migrating operator brings with
-- them: namespaced database names (`s{short}_{name}`), generated
-- usernames (`u{short}_{rand}`), the allowed connection source
-- (`remote`, `%` = any), and the per-user connection cap.
--
-- The honest boundary (recorded in the domain module's docs): the
-- engine stores and manages these rows — it does not connect to
-- the host to issue CREATE DATABASE / CREATE USER. That is the
-- workloads/database integration's milestone; these rows become
-- its input then.
--
-- Conventions (per 0001): UUID ids -> TEXT (canonical hyphenated),
-- timestamps -> TEXT in RFC 3339 / UTC, booleans -> INTEGER 0/1,
-- nullable caps -> INTEGER NULL.
--
-- Uniqueness: `name` and `username` are globally unique — the
-- Panel's own constraint (a shared host cannot carry two
-- databases of the same name; a username is a host-level
-- principal). Host names are unique too (operator-facing labels
-- must not collide).
--
-- Deletion posture: databases cascade with their instance (the
-- per-instance convention). The host tie is the OPPOSITE posture:
-- a host with live databases refuses deletion at the store level
-- (plain FK, no cascade) — the API layer turns that into a
-- structured 409 naming the count, and the operator decides. A
-- silent cascade would destroy credentials and connection records
-- the operator has not agreed to lose — the same explicit-refusal
-- discipline the allocation and subuser families follow.

CREATE TABLE database_hosts (
    id              TEXT    PRIMARY KEY,
    name            TEXT    NOT NULL UNIQUE,
    host            TEXT    NOT NULL,
    port            INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    username        TEXT    NOT NULL,
    password        TEXT    NOT NULL,
    max_databases   INTEGER,
    node_id         TEXT    REFERENCES nodes (id) ON DELETE SET NULL,
    created_at      TEXT    NOT NULL,
    updated_at      TEXT    NOT NULL
);

CREATE TABLE databases (
    id              TEXT    PRIMARY KEY,
    instance_id     TEXT    NOT NULL REFERENCES instances (id) ON DELETE CASCADE,
    host_id         TEXT    NOT NULL REFERENCES database_hosts (id),
    name            TEXT    NOT NULL UNIQUE,
    username        TEXT    NOT NULL UNIQUE,
    password        TEXT    NOT NULL,
    remote          TEXT    NOT NULL DEFAULT '%',
    max_connections INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT    NOT NULL,
    updated_at      TEXT    NOT NULL
);

CREATE INDEX idx_databases_instance ON databases (instance_id);
CREATE INDEX idx_databases_host ON databases (host_id);
