-- 0003_inherited_dep_refs — dep-refs learned a fourth `ref_kind` in #43
-- (Cargo `{ workspace = true }` inheritance, distinct from a path or a git
-- reference) and nothing downstream was told: every sweep since aborted on
-- this table's CHECK constraint (#44).
--
-- SQLite cannot ALTER a CHECK constraint, so the table is rebuilt: create the
-- replacement under a temporary name, copy what is there, drop the old one,
-- rename into place. `latest_dep_refs` is a droppable projection (NFR-I4,
-- see 0002_evidence.sql) — the copy is not required for correctness, since
-- the next successful sweep repopulates it from scratch via
-- `replace_dep_refs`, but doing it anyway means a database is never left
-- with an empty projection between this migration and that sweep.

CREATE TABLE latest_dep_refs_new (
    subject_key     TEXT PRIMARY KEY NOT NULL,
    from_project_id TEXT NOT NULL,
    ref_kind        TEXT NOT NULL CHECK (
        ref_kind IN ('path', 'git', 'declared', 'inherited')
    ),
    raw_target      TEXT NOT NULL,
    manifest        TEXT,
    to_project_id   TEXT,
    observed_at     TEXT NOT NULL
);

INSERT INTO latest_dep_refs_new
    SELECT subject_key, from_project_id, ref_kind, raw_target, manifest,
           to_project_id, observed_at
    FROM latest_dep_refs;

DROP TABLE latest_dep_refs;

ALTER TABLE latest_dep_refs_new RENAME TO latest_dep_refs;

CREATE INDEX idx_dep_refs_from ON latest_dep_refs (from_project_id);
CREATE INDEX idx_dep_refs_to ON latest_dep_refs (to_project_id);
