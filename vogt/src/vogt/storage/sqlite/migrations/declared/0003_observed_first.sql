-- 0003_observed_first — the two declared tables observed-first needs.
--
-- Both live in the *declared* store on purpose. A suppression and an
-- adoption are audited human or agent decisions, not observations — which is
-- also why a suppression survives re-observation of the same subject, and
-- why an observation-store dismissal could never have worked.

-- FR-W10. Removes a subject from ranked and aggregated views permanently;
-- it stays observable and queryable. A first-class operation rather than
-- `adopt` + `wont_do`, because the latter fabricates a declared work item
-- for every piece of noise (DESIGN §3.6).
CREATE TABLE suppressions (
    id                     TEXT PRIMARY KEY NOT NULL,
    match_kind             TEXT NOT NULL CHECK (match_kind IN ('exact', 'pattern')),
    subject_key_or_pattern TEXT NOT NULL CHECK (
        length(trim(subject_key_or_pattern)) > 0
    ),
    scope_project_id       TEXT REFERENCES projects (id),
    actor_id               TEXT NOT NULL REFERENCES actors (id),
    reason                 TEXT NOT NULL CHECK (length(trim(reason)) > 0),
    created_at             TEXT NOT NULL,
    revoked_at             TEXT,
    revoked_by_actor_id    TEXT REFERENCES actors (id),
    revoked_reason         TEXT
);

CREATE INDEX idx_suppressions_live ON suppressions (revoked_at, match_kind);

-- FR-W5. The maintained link from an adopted work item back to what was
-- observed, so drift can keep the pair honest from M3.
--
-- Deviation from SCHEMA.md §2.3, recorded: that draft keyed this on
-- (forge_kind, repo, number). This keys on `subject_key`, because `adopt`
-- applies to any observed subject — a promoted source marker as much as a
-- GitHub issue — and the subject key is the one identifier every collector
-- already produces.
CREATE TABLE work_links (
    work_item_id TEXT NOT NULL REFERENCES work_items (id),
    subject_key  TEXT NOT NULL,
    origin_kind  TEXT NOT NULL,
    source_url   TEXT,
    relation     TEXT NOT NULL CHECK (relation IN ('completion', 'reference')),
    created_at   TEXT NOT NULL,
    PRIMARY KEY (work_item_id, subject_key)
);

CREATE INDEX idx_work_links_subject ON work_links (subject_key);
