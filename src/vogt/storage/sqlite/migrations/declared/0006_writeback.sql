-- 0006_writeback — the per-project write-back policy, and its ledger.
--
-- FR-B1: governed per project, defaulting to `none`. A tool that can write
-- to somebody's issue tracker should do so only where it has been told to,
-- one repository at a time.
--
-- FR-B4 is enforced by what this schema can express as much as by code:
-- there is no column for a deletion, a force, or a history rewrite, because
-- those are not capabilities the product has. Write-back is additive and
-- forward-only under every policy level — create, comment, label,
-- close/reopen — and that is the whole list.

ALTER TABLE projects ADD COLUMN write_back TEXT NOT NULL DEFAULT 'none'
    CHECK (write_back IN ('none', 'comment_only', 'full'));

-- Every write-back action, recorded before it is attempted and updated with
-- what happened (FR-B2). Kept separate from `audit` because an audit row
-- describes a change to *our* declared data; this describes a change we made
-- to somebody else's system, which can fail after we committed.
--
-- The next sweep re-observes the result, so Vogt sees its own writes the
-- same way it sees anyone else's. That loop is the point: a write-back that
-- is never re-observed is a claim, not a fact.
CREATE TABLE writeback_actions (
    id            TEXT PRIMARY KEY NOT NULL,
    at            TEXT NOT NULL,
    project_id    TEXT REFERENCES projects (id),
    work_item_id  TEXT REFERENCES work_items (id),
    actor_id      TEXT NOT NULL REFERENCES actors (id),
    action        TEXT NOT NULL CHECK (
        action IN ('create', 'comment', 'label', 'close', 'reopen')
    ),
    subject_key   TEXT,
    policy        TEXT NOT NULL,
    outcome       TEXT NOT NULL CHECK (
        outcome IN ('attempted', 'succeeded', 'failed', 'skipped')
    ),
    reason        TEXT NOT NULL,
    detail        TEXT,
    source_url    TEXT
);

CREATE INDEX idx_writeback_at ON writeback_actions (at);
CREATE INDEX idx_writeback_item ON writeback_actions (work_item_id);
CREATE INDEX idx_writeback_outcome ON writeback_actions (outcome);
