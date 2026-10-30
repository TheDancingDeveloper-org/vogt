-- 0029_comment_verdict — a comment may also be a review verdict or a
-- verification (WI-1319, design agent-harness-services §2.2).
--
-- SQLite cannot widen a CHECK in place, so the table is rebuilt with the
-- same columns and index. Nothing references `comments`, so the drop is
-- safe with foreign keys on. The new kinds keep their structured fields in
-- `data`, as a hand-off does.
CREATE TABLE comments_new (
    id           TEXT PRIMARY KEY NOT NULL,
    work_item_id TEXT NOT NULL REFERENCES work_items (id),
    actor_id     TEXT NOT NULL REFERENCES actors (id),
    body         TEXT NOT NULL CHECK (length(trim(body)) > 0),
    created_at   TEXT NOT NULL,
    kind         TEXT NOT NULL DEFAULT 'note'
        CHECK (kind IN ('note', 'handoff', 'verdict', 'verification')),
    data         TEXT
);

INSERT INTO comments_new (id, work_item_id, actor_id, body, created_at, kind, data)
    SELECT id, work_item_id, actor_id, body, created_at, kind, data FROM comments;

DROP TABLE comments;

ALTER TABLE comments_new RENAME TO comments;

CREATE INDEX idx_comments_work_item ON comments (work_item_id, created_at);
