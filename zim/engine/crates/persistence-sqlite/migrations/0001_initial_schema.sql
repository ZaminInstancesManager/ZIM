-- 0001_initial_schema.sql — the complete M3 entity schema.
--
-- Mirrors the domain model in docs/architecture/RUST_ENGINE.md §3
-- with the storage shaping from §5:
--   * foreign keys between entities;
--   * indexes on the hot query paths named in §5;
--   * desired state only on `instances` (observed runtime state is
--     the supervision module's concern, never persisted as proof of
--     liveness).
--
-- Column conventions:
--   * UUID ids -> TEXT (canonical hyphenated form);
--   * AllocationId -> INTEGER (Pterodactyl auto-increment compat);
--   * timestamps -> TEXT in RFC 3339 / UTC;
--   * booleans -> INTEGER 0/1;
--   * enums -> TEXT in their serde snake_case vocabulary;
--   * structured values (limits, capabilities, grants, actions,
--     metadata, manifests, signatures) -> TEXT JSON.
--
-- The runner (src/migrations.rs) wraps each migration in a
-- transaction with foreign keys disabled during application, so the
-- table creation order below is chosen for readability, not for FK
-- dependency order.

-- ─── Locations & nodes ────────────────────────────────────────────

CREATE TABLE nodes (
    id               TEXT    PRIMARY KEY,
    name             TEXT    NOT NULL,
    location_id      TEXT    NOT NULL,
    fqdn             TEXT    NOT NULL,
    scheme           TEXT    NOT NULL CHECK (scheme IN ('http', 'https')),
    public_port      INTEGER NOT NULL CHECK (public_port >= 0 AND public_port <= 65535),
    listen_port      INTEGER NOT NULL CHECK (listen_port >= 1 AND listen_port <= 65535),
    capabilities     TEXT    NOT NULL,  -- JSON NodeCapabilities
    deployment       TEXT    NOT NULL CHECK (deployment IN ('local_linux', 'remote_linux')),
    last_heartbeat   TEXT,               -- RFC 3339, NULL = never
    last_seen_state  TEXT    NOT NULL CHECK (last_seen_state IN ('online', 'offline', 'unknown'))
);

-- ─── Users & permissions ──────────────────────────────────────────

CREATE TABLE users (
    id             TEXT    PRIMARY KEY,
    email          TEXT    NOT NULL UNIQUE COLLATE NOCASE,
    username       TEXT    NOT NULL UNIQUE,
    external_id    TEXT    UNIQUE,
    password_hash  TEXT,
    oauth_subject  TEXT    UNIQUE,
    is_admin       INTEGER NOT NULL DEFAULT 0,
    root_admin     INTEGER NOT NULL DEFAULT 0
);

-- ─── Allocations ──────────────────────────────────────────────────

CREATE TABLE allocations (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    node_id       TEXT    NOT NULL REFERENCES nodes (id),
    ip            TEXT    NOT NULL,
    port          INTEGER NOT NULL CHECK (port >= 1 AND port <= 65535),
    assigned_to   TEXT    REFERENCES instances (id),
    note          TEXT,
    UNIQUE (node_id, ip, port)
);

-- ─── Instances ────────────────────────────────────────────────────

CREATE TABLE instances (
    id                      TEXT    PRIMARY KEY,
    external_id             TEXT    UNIQUE,
    uuid                    TEXT    NOT NULL UNIQUE,
    uuid_short              TEXT    NOT NULL UNIQUE,
    node_id                 TEXT    NOT NULL REFERENCES nodes (id),
    name                    TEXT    NOT NULL,
    description             TEXT    NOT NULL DEFAULT '',
    status                  TEXT    NOT NULL CHECK (status IN (
        'offline', 'starting', 'running', 'stopping', 'installing',
        'suspended', 'restoring_backup', 'transferring', 'crash_loop',
        'unknown')),
    skip_egg_scripts        INTEGER NOT NULL DEFAULT 0,
    owner_id                TEXT    NOT NULL REFERENCES users (id),
    limits                  TEXT    NOT NULL,  -- JSON ResourceLimits
    allocation_id           INTEGER NOT NULL REFERENCES allocations (id),
    egg_id                  TEXT    NOT NULL,
    startup                 TEXT    NOT NULL,
    image                   TEXT    NOT NULL,
    allocation_limit        INTEGER,
    database_limit          INTEGER,
    backup_limit            INTEGER NOT NULL DEFAULT 0,
    crash_detection_enabled INTEGER NOT NULL DEFAULT 0,
    installed_at            TEXT,
    created_at              TEXT    NOT NULL,
    updated_at              TEXT    NOT NULL
);

CREATE INDEX idx_instances_node  ON instances (node_id);
CREATE INDEX idx_instances_owner ON instances (owner_id);

CREATE TABLE permissions (
    user_id      TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    instance_id  TEXT NOT NULL REFERENCES instances (id) ON DELETE CASCADE,
    grants       TEXT NOT NULL,  -- JSON array of grant strings
    PRIMARY KEY (user_id, instance_id)
);

CREATE INDEX idx_permissions_instance ON permissions (instance_id);

-- ─── Jobs, backups, transfers ─────────────────────────────────────

CREATE TABLE jobs (
    id               TEXT    PRIMARY KEY,
    kind             TEXT    NOT NULL CHECK (kind IN ('backup', 'transfer', 'install', 'update')),
    instance_id      TEXT    NOT NULL REFERENCES instances (id) ON DELETE CASCADE,
    state            TEXT    NOT NULL CHECK (state IN (
        'pending', 'in_progress', 'succeeded', 'failed', 'cancelled',
        'cleanup_pending')),
    created_at       TEXT    NOT NULL,
    updated_at       TEXT    NOT NULL,
    started_at       TEXT,
    completed_at     TEXT,
    last_error       TEXT,
    cleanup_pending  INTEGER NOT NULL DEFAULT 0,
    generation       INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_jobs_state_instance ON jobs (state, instance_id);

CREATE TABLE backups (
    id            TEXT    PRIMARY KEY,
    instance_id   TEXT    NOT NULL REFERENCES instances (id) ON DELETE CASCADE,
    job_id        TEXT    NOT NULL REFERENCES jobs (id),
    uuid          TEXT    NOT NULL UNIQUE,
    file_name     TEXT    NOT NULL,
    bytes         INTEGER NOT NULL,
    sha256_hash   TEXT,
    locked        INTEGER NOT NULL DEFAULT 0,
    completed_at  TEXT
);

CREATE INDEX idx_backups_instance ON backups (instance_id);

CREATE TABLE transfers (
    id            TEXT    PRIMARY KEY,
    instance_id   TEXT    NOT NULL REFERENCES instances (id) ON DELETE CASCADE,
    job_id        TEXT    NOT NULL REFERENCES jobs (id),
    from_node_id  TEXT    NOT NULL REFERENCES nodes (id),
    to_node_id    TEXT    NOT NULL REFERENCES nodes (id),
    state         TEXT    NOT NULL CHECK (state IN (
        'pending', 'source_stopping', 'source_archiving',
        'archive_transferring', 'dest_extracting', 'dest_starting',
        'source_cleanup', 'succeeded', 'failed', 'cancelled')),
    archive_path  TEXT
);

CREATE INDEX idx_transfers_instance ON transfers (instance_id);

-- ─── Schedules ────────────────────────────────────────────────────

CREATE TABLE schedules (
    id                TEXT    PRIMARY KEY,
    instance_id       TEXT    NOT NULL REFERENCES instances (id) ON DELETE CASCADE,
    name              TEXT    NOT NULL,
    cron              TEXT    NOT NULL,
    next_run_at       TEXT,
    last_run_at       TEXT,
    is_active         INTEGER NOT NULL DEFAULT 0,
    only_when_online  INTEGER NOT NULL DEFAULT 0,
    actions           TEXT    NOT NULL  -- JSON array of ScheduleAction
);

CREATE INDEX idx_schedules_instance  ON schedules (instance_id);
CREATE INDEX idx_schedules_next_run  ON schedules (is_active, next_run_at);

-- ─── Audit (append-only) ──────────────────────────────────────────

CREATE TABLE audit (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp    TEXT    NOT NULL,
    actor        TEXT    NOT NULL CHECK (actor IN ('user', 'extension', 'system')),
    actor_id     TEXT,
    action       TEXT    NOT NULL,
    instance_id  TEXT    REFERENCES instances (id) ON DELETE SET NULL,
    metadata     TEXT    NOT NULL,  -- JSON object
    ip_address   TEXT,
    user_agent   TEXT
);

CREATE INDEX idx_audit_timestamp ON audit (timestamp);

-- ─── Extension registrations ──────────────────────────────────────

CREATE TABLE extension_registrations (
    id                   TEXT    PRIMARY KEY,
    manifest             TEXT    NOT NULL,  -- JSON ExtensionManifest
    version              TEXT    NOT NULL,
    installed_at         TEXT    NOT NULL,
    enabled              INTEGER NOT NULL DEFAULT 0,
    granted_permissions  TEXT    NOT NULL,  -- JSON array of grant strings
    signature            TEXT               -- JSON ExtensionSignature, NULL = unsigned
);
