-- 0026_session_reports — a session's own typed report of where it is (WI-2039).
--
-- A worker posts a short structured message — progress, result, question or
-- blocker — instead of leaving oversight to scrape its transcript. Keyed by
-- the reporting engine session, so the sweep and the digest find the latest
-- one for every row, linked or not. The work item is the session's binding
-- when the report was made (or the one it named). The structured fields
-- (branch, sha, pr, checks, blockers, next step) are one JSON document, as
-- on a hand-off comment: a field added later is a new key, not a migration.
CREATE TABLE session_reports (
    id                TEXT PRIMARY KEY,
    actor_id          TEXT NOT NULL REFERENCES actors (id),
    engine_session_id TEXT,
    work_item_id      TEXT REFERENCES work_items (id),
    kind              TEXT NOT NULL
        CHECK (kind IN ('progress', 'result', 'question', 'blocker')),
    summary           TEXT NOT NULL CHECK (length(trim(summary)) > 0),
    data              TEXT NOT NULL DEFAULT '{}',
    created_at        TEXT NOT NULL
);

CREATE INDEX idx_session_reports_session
    ON session_reports (engine_session_id, created_at);
CREATE INDEX idx_session_reports_created ON session_reports (created_at);
CREATE INDEX idx_session_reports_work_item ON session_reports (work_item_id);
