-- 0002_evidence — the append-only evidence store and its derived tables.
--
-- Two disciplines meet here. `sweeps` and `observations` are immutable
-- history: collectors append, nothing updates, and the only delete path is
-- retention (§5 of SCHEMA.md). `latest_*` are derived projections, rebuilt
-- transactionally at sweep completion and droppable at any time (NFR-I4).
--
-- Deviation from SCHEMA.md §3.2, recorded deliberately: that draft named
-- five typed `latest_*` tables (forge items, CI runs, releases, markers,
-- dep refs). This ships two — a generic `latest_observations` keyed by
-- subject, plus `latest_dep_refs` — because only the dependency projection
-- carries anything the observation does not already say. The other four
-- differed from each other in their payload shape and not in their
-- behaviour, so five rebuild paths would have been five places for a
-- collector and its projection to drift apart. Typed reads are queries over
-- `kind`, and payload JSON holds the type-specific fields.

CREATE TABLE sweeps (
    id          TEXT PRIMARY KEY NOT NULL,
    collector   TEXT NOT NULL,
    scope       TEXT NOT NULL,
    started_at  TEXT NOT NULL,
    finished_at TEXT,
    outcome     TEXT NOT NULL CHECK (
        outcome IN ('running', 'ok', 'partial', 'failed')
    ),
    stats       TEXT NOT NULL DEFAULT '{}',
    detail      TEXT
);

CREATE INDEX idx_sweeps_collector ON sweeps (collector, started_at);

-- One immutable evidence row. `subject_key` is the deterministic natural key
-- (`gh:owner/repo#123`, `mark:slug/path#L42`, `depref:slug/Cargo.toml→...`);
-- same subject + same digest in a later sweep writes nothing (FR-O7), which
-- is what keeps growth proportional to change rather than to how often we
-- look (NFR-S2).
CREATE TABLE observations (
    id             TEXT PRIMARY KEY NOT NULL,
    sweep_id       TEXT NOT NULL REFERENCES sweeps (id),
    collector      TEXT NOT NULL,
    kind           TEXT NOT NULL,
    project_id     TEXT,
    subject_key    TEXT NOT NULL,
    payload        TEXT NOT NULL,
    content_digest TEXT NOT NULL,
    source_url     TEXT,
    -- Derived from the FR-W11 promotion pattern at collection time, so an
    -- unpromoted marker stays observable and queryable without claiming to
    -- be work.
    promoted       INTEGER NOT NULL DEFAULT 0 CHECK (promoted IN (0, 1)),
    observed_at    TEXT NOT NULL
);

CREATE INDEX idx_observations_subject ON observations (subject_key, observed_at);
CREATE INDEX idx_observations_project ON observations (project_id, kind);
CREATE INDEX idx_observations_sweep ON observations (sweep_id);

-- Newest observation per subject. Rebuilt from `observations`; never a
-- source of truth (NFR-I4).
CREATE TABLE latest_observations (
    subject_key    TEXT PRIMARY KEY NOT NULL,
    observation_id TEXT NOT NULL REFERENCES observations (id),
    collector      TEXT NOT NULL,
    kind           TEXT NOT NULL,
    project_id     TEXT,
    payload        TEXT NOT NULL,
    content_digest TEXT NOT NULL,
    source_url     TEXT,
    promoted       INTEGER NOT NULL DEFAULT 0 CHECK (promoted IN (0, 1)),
    observed_at    TEXT NOT NULL
);

CREATE INDEX idx_latest_kind ON latest_observations (kind, project_id);
CREATE INDEX idx_latest_promoted ON latest_observations (kind, promoted);

-- One row per (project, target) reference (FR-D1–D4). `to_project_id` is
-- resolved against the registered project list at rebuild time; an
-- internal-looking reference that resolves to nothing keeps its raw target
-- and is reported as `unresolved_dependency` from M3.
CREATE TABLE latest_dep_refs (
    subject_key     TEXT PRIMARY KEY NOT NULL,
    from_project_id TEXT NOT NULL,
    ref_kind        TEXT NOT NULL CHECK (ref_kind IN ('path', 'git', 'declared')),
    raw_target      TEXT NOT NULL,
    manifest        TEXT,
    to_project_id   TEXT,
    observed_at     TEXT NOT NULL
);

CREATE INDEX idx_dep_refs_from ON latest_dep_refs (from_project_id);
CREATE INDEX idx_dep_refs_to ON latest_dep_refs (to_project_id);
