CREATE TABLE session_reports (
    id TEXT PRIMARY KEY,
    actor_id TEXT NOT NULL REFERENCES actors(id),
    engine_session_id TEXT,
    work_item_id TEXT REFERENCES work_items(id),
    kind TEXT NOT NULL CHECK (kind IN ('progress', 'result', 'question', 'blocker')),
    summary TEXT NOT NULL CHECK (length(trim(summary)) > 0),
    fields TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL
);

CREATE INDEX idx_session_reports_session_latest
    ON session_reports(engine_session_id, created_at DESC, id DESC);
CREATE INDEX idx_session_reports_work_item
    ON session_reports(work_item_id, created_at DESC);
