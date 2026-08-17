-- 0008_inbox_triage — shared occurrence-scoped Inbox decisions.
--
-- This is a projection of a person's current triage decision. The audit and
-- event tables retain the complete history; the bounded snapshot means an
-- archived occurrence remains explainable even if its live source changes.

CREATE TABLE inbox_triage (
    entry_key           TEXT PRIMARY KEY NOT NULL,
    state               TEXT NOT NULL CHECK (state IN ('active', 'archived', 'snoozed')),
    snooze_until        TEXT,
    actor_id            TEXT NOT NULL REFERENCES actors (id),
    decided_at          TEXT NOT NULL,
    occurrence_snapshot TEXT NOT NULL DEFAULT '{}'
);

CREATE INDEX idx_inbox_triage_state ON inbox_triage (state, decided_at);
CREATE INDEX idx_inbox_triage_decided ON inbox_triage (decided_at, entry_key);
