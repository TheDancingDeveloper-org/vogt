-- WI-4638 slice 3: FTS5 text search over work items and comments.
--
-- External-content FTS5 tables with the trigram tokenizer: a MATCH on a quoted
-- phrase is a case-insensitive substring match, the same meaning as the
-- instr(lower(...)) scan it replaces, but answered from an index. Triggers keep
-- the index in step with the base tables. The engine falls back to the scan
-- when these tables are absent (a SQLite build without FTS5, where the migrator
-- records this migration as applied without effect).
CREATE VIRTUAL TABLE work_items_fts USING fts5(
    title, body, ref,
    content = 'work_items', content_rowid = 'rowid', tokenize = 'trigram'
);

CREATE VIRTUAL TABLE comments_fts USING fts5(
    body,
    content = 'comments', content_rowid = 'rowid', tokenize = 'trigram'
);

CREATE TRIGGER work_items_fts_ai AFTER INSERT ON work_items BEGIN
    INSERT INTO work_items_fts (rowid, title, body, ref)
        VALUES (new.rowid, new.title, new.body, new.ref);
END;

CREATE TRIGGER work_items_fts_ad AFTER DELETE ON work_items BEGIN
    INSERT INTO work_items_fts (work_items_fts, rowid, title, body, ref)
        VALUES ('delete', old.rowid, old.title, old.body, old.ref);
END;

CREATE TRIGGER work_items_fts_au AFTER UPDATE OF title, body, ref ON work_items BEGIN
    INSERT INTO work_items_fts (work_items_fts, rowid, title, body, ref)
        VALUES ('delete', old.rowid, old.title, old.body, old.ref);
    INSERT INTO work_items_fts (rowid, title, body, ref)
        VALUES (new.rowid, new.title, new.body, new.ref);
END;

CREATE TRIGGER comments_fts_ai AFTER INSERT ON comments BEGIN
    INSERT INTO comments_fts (rowid, body) VALUES (new.rowid, new.body);
END;

CREATE TRIGGER comments_fts_ad AFTER DELETE ON comments BEGIN
    INSERT INTO comments_fts (comments_fts, rowid, body)
        VALUES ('delete', old.rowid, old.body);
END;

CREATE TRIGGER comments_fts_au AFTER UPDATE OF body ON comments BEGIN
    INSERT INTO comments_fts (comments_fts, rowid, body)
        VALUES ('delete', old.rowid, old.body);
    INSERT INTO comments_fts (rowid, body) VALUES (new.rowid, new.body);
END;

-- Backfill the rows that exist already.
INSERT INTO work_items_fts (work_items_fts) VALUES ('rebuild');

INSERT INTO comments_fts (comments_fts) VALUES ('rebuild');
