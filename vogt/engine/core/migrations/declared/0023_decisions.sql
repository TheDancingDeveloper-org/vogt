-- 0023_decisions — questions an agent puts to a person (WI-1321).
--
-- Shaped like session_grants (0019): an agent asks, a person answers from the
-- Inbox, and nothing is applied when it is answered. `expired` is not stored;
-- it is a pending row whose `expires_at` has passed, computed on read, and it
-- implies no answer. Options and the answer are JSON.
CREATE TABLE decisions (
    id                   TEXT PRIMARY KEY,
    asker_actor_id       TEXT NOT NULL REFERENCES actors (id),
    asker_session        TEXT,
    work_item_id         TEXT REFERENCES work_items (id),
    question             TEXT NOT NULL,
    options_json         TEXT NOT NULL,
    context              TEXT NOT NULL DEFAULT '',
    state                TEXT NOT NULL
        CHECK (state IN ('pending', 'answered', 'withdrawn')),
    answer_json          TEXT,
    answered_by_actor_id TEXT REFERENCES actors (id),
    created_at           TEXT NOT NULL,
    answered_at          TEXT,
    expires_at           TEXT NOT NULL,
    reason               TEXT NOT NULL
);

CREATE INDEX idx_decisions_state ON decisions (state, created_at);
CREATE INDEX idx_decisions_work_item ON decisions (work_item_id);
