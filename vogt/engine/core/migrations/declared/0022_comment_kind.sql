-- 0022_comment_kind — a comment is a note or a hand-off.
--
-- `kind` distinguishes an ordinary remark from the structured hand-off a
-- fresh session starts from (design C11). Existing rows are notes. `data`
-- holds the hand-off fields as one JSON document, the same shape as a
-- project's agent policy: a fact added later is a new key, not another
-- migration. A note stores nothing there.
ALTER TABLE comments ADD COLUMN kind TEXT NOT NULL DEFAULT 'note'
    CHECK (kind IN ('note', 'handoff'));
ALTER TABLE comments ADD COLUMN data TEXT;
