-- M23 NTP quick-access: per-user recent servers.
--
-- Engine-backed app state per BROWSER_INTEGRATION.md §11 (not a
-- browser-history database): one row per (user, instance) with the
-- last-opened timestamp and an open counter. The store caps rows at
-- the newest 32 per user on write; rows for deleted instances are
-- harmless orphans the read path (and the frontend) filter.
--
-- Conventions (per 0001): UUID ids -> TEXT (canonical hyphenated),
-- timestamps -> TEXT in RFC 3339 / UTC, counters -> INTEGER.

CREATE TABLE user_recents (
    user_id        TEXT    NOT NULL,
    instance_id    TEXT    NOT NULL,
    last_opened_at TEXT    NOT NULL,
    open_count     INTEGER NOT NULL CHECK (open_count >= 1),
    PRIMARY KEY (user_id, instance_id)
);

CREATE INDEX idx_user_recents_recency
    ON user_recents (user_id, last_opened_at DESC);
