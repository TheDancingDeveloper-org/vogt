-- 0027_watches — a session waits on an event instead of polling (WI-2036).
--
-- A watch names a trigger (a pull request's state, CI on a revision, a work
-- item's state, another session ending) and the session to wake when it
-- fires. It is a row, not a timer, so it survives an engine or core restart:
-- the core's watch loop reads the active rows each pass. `trigger_json` and
-- `outcome_json` are JSON; `kind` is the trigger's kind, copied out so the
-- loop can read by kind.
CREATE TABLE watches (
    id                TEXT PRIMARY KEY,
    owner_actor_id    TEXT NOT NULL REFERENCES actors (id),
    target_session    TEXT NOT NULL,
    kind              TEXT NOT NULL,
    trigger_json      TEXT NOT NULL,
    note              TEXT NOT NULL DEFAULT '',
    state             TEXT NOT NULL
        CHECK (state IN ('active', 'fired', 'failed', 'cancelled', 'expired')),
    created_at        TEXT NOT NULL,
    expires_at        TEXT NOT NULL,
    fired_at          TEXT,
    outcome_json      TEXT,
    attempts          INTEGER NOT NULL DEFAULT 0,
    last_error        TEXT,
    reason            TEXT NOT NULL
);

CREATE INDEX idx_watches_state ON watches (state, created_at);
CREATE INDEX idx_watches_target ON watches (target_session);
