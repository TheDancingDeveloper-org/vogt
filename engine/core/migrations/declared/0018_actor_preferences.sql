-- 0018_actor_preferences — small per-actor settings that follow a login.
--
-- One row per (actor, key). `value` is a JSON object the owning surface
-- defines (the Inbox stores its saved filter under `inbox.filter`), and
-- `version` increments on every write so a client can tell its cached copy
-- is behind, and can ask for a write to apply only on top of what it read.
-- A preference is a declared write like any other: every change lands an
-- audit row and an event in the same transaction, so this is the current
-- value and the audit table is its history.
CREATE TABLE actor_preferences (
    actor_id   TEXT NOT NULL REFERENCES actors (id),
    key        TEXT NOT NULL CHECK (length(key) BETWEEN 1 AND 64),
    value      TEXT NOT NULL DEFAULT '{}',
    version    INTEGER NOT NULL CHECK (version >= 1),
    updated_at TEXT NOT NULL,
    PRIMARY KEY (actor_id, key)
);
