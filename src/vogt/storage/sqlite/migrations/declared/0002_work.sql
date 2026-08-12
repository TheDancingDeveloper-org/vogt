-- 0002_work — the write plane: work items, relations, labels, initiatives,
-- comments, and the per-kind workflow definitions that govern transitions.
--
-- There is deliberately no `rank_order` column (decided 2026-08-12,
-- `SCHEMA.md` §2.3). Ordering is computed from documented weights and is
-- fully explainable by `why`; manual influence goes through `priority` and
-- initiative weight, which are themselves scored inputs. A hand-set position
-- competing with the score is how a ranking stops being explainable.

CREATE TABLE initiatives (
    id         TEXT PRIMARY KEY NOT NULL,
    slug       TEXT NOT NULL UNIQUE,
    title      TEXT NOT NULL,
    body       TEXT NOT NULL DEFAULT '',
    state      TEXT NOT NULL CHECK (state IN ('open', 'closed')),
    weight     INTEGER NOT NULL DEFAULT 0 CHECK (weight BETWEEN 0 AND 100),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE labels (
    id         TEXT PRIMARY KEY NOT NULL,
    name       TEXT NOT NULL UNIQUE,
    color      TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE work_items (
    id                TEXT PRIMARY KEY NOT NULL,
    ref               TEXT NOT NULL UNIQUE,
    kind              TEXT NOT NULL CHECK (
        kind IN ('feature', 'bug', 'chore', 'question')
    ),
    title             TEXT NOT NULL,
    body              TEXT NOT NULL DEFAULT '',
    state             TEXT NOT NULL,
    priority          TEXT NOT NULL CHECK (
        priority IN ('p0', 'p1', 'p2', 'p3', 'p4')
    ),
    effort            TEXT CHECK (effort IN ('xs', 's', 'm', 'l', 'xl')),
    project_id        TEXT REFERENCES projects (id),
    initiative_id     TEXT REFERENCES initiatives (id),
    origin            TEXT NOT NULL CHECK (
        origin IN ('created', 'adopted', 'observed')
    ),
    trust_state       TEXT NOT NULL CHECK (
        trust_state IN ('verified', 'stale', 'unverified', 'disputed')
    ),
    assignee_actor_id TEXT REFERENCES actors (id),
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL
);

CREATE INDEX idx_work_items_project ON work_items (project_id);
CREATE INDEX idx_work_items_state ON work_items (state);
CREATE INDEX idx_work_items_kind ON work_items (kind);
CREATE INDEX idx_work_items_assignee ON work_items (assignee_actor_id);
CREATE INDEX idx_work_items_initiative ON work_items (initiative_id);

-- Typed, cross-project edges (FR-W8), aligned with GitHub issue relation
-- semantics so observed forge relations map losslessly at M5.
CREATE TABLE work_relations (
    work_item_id TEXT NOT NULL REFERENCES work_items (id),
    related_id   TEXT NOT NULL REFERENCES work_items (id),
    kind         TEXT NOT NULL CHECK (
        kind IN ('depends_on', 'relates_to', 'duplicate_of', 'parent_of')
    ),
    created_at   TEXT NOT NULL,
    PRIMARY KEY (work_item_id, related_id, kind),
    CHECK (work_item_id <> related_id)
);

CREATE INDEX idx_work_relations_related ON work_relations (related_id, kind);

CREATE TABLE work_item_labels (
    work_item_id TEXT NOT NULL REFERENCES work_items (id),
    label_id     TEXT NOT NULL REFERENCES labels (id),
    PRIMARY KEY (work_item_id, label_id)
);

CREATE INDEX idx_work_item_labels_label ON work_item_labels (label_id);

CREATE TABLE comments (
    id           TEXT PRIMARY KEY NOT NULL,
    work_item_id TEXT NOT NULL REFERENCES work_items (id),
    actor_id     TEXT NOT NULL REFERENCES actors (id),
    body         TEXT NOT NULL CHECK (length(trim(body)) > 0),
    created_at   TEXT NOT NULL
);

CREATE INDEX idx_comments_work_item ON comments (work_item_id, created_at);

-- One state machine per work-item kind (FR-W2). Held as data rather than
-- code so that changing a workflow is configuration, and so a rejected
-- transition can name the rule it violated from the same source the
-- transition was checked against.
CREATE TABLE workflow_defs (
    kind       TEXT PRIMARY KEY NOT NULL CHECK (
        kind IN ('feature', 'bug', 'chore', 'question')
    ),
    definition TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
