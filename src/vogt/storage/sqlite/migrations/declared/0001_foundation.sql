-- 0001_foundation — the M0 spine: instance metadata, identity, audit,
-- events, and the project record the M0 demo registers.
--
-- Portability note (NFR-S3): AUTOINCREMENT on events.seq is SQLite's
-- spelling of a monotonic sequence that never reuses a value after a
-- delete. Postgres spells the same guarantee GENERATED ALWAYS AS IDENTITY.
-- Nothing above the storage interface knows which is in use.

CREATE TABLE meta (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);

CREATE TABLE actors (
    id           TEXT PRIMARY KEY NOT NULL,
    kind         TEXT NOT NULL CHECK (kind IN ('human', 'agent')),
    display_name TEXT NOT NULL,
    identity_ref TEXT NOT NULL UNIQUE,
    disabled     INTEGER NOT NULL DEFAULT 0 CHECK (disabled IN (0, 1)),
    created_at   TEXT NOT NULL
);

CREATE TABLE projects (
    id                    TEXT PRIMARY KEY NOT NULL,
    slug                  TEXT NOT NULL UNIQUE,
    name                  TEXT NOT NULL,
    root_path             TEXT NOT NULL,
    repo_url              TEXT,
    lifecycle_state       TEXT NOT NULL CHECK (
        lifecycle_state IN ('incubating', 'active', 'maintenance', 'archived')
    ),
    current_version       TEXT,
    contract_version      TEXT,
    compliance_status     TEXT NOT NULL CHECK (
        compliance_status IN ('compliant', 'non_compliant', 'not_checked')
    ),
    compliance_checked_at TEXT,
    exclusions            TEXT NOT NULL DEFAULT '[]',
    trust_state           TEXT NOT NULL CHECK (
        trust_state IN ('verified', 'stale', 'unverified', 'disputed')
    ),
    created_at            TEXT NOT NULL,
    updated_at            TEXT NOT NULL
);

-- Every declared write, explained (FR-S1). The CHECK is the last line of
-- defence for the rule that a reason may not be blank: an empty reason
-- records that something happened while explaining nothing.
CREATE TABLE audit (
    id             TEXT PRIMARY KEY NOT NULL,
    txn_id         TEXT NOT NULL,
    revision       INTEGER NOT NULL,
    actor_id       TEXT NOT NULL REFERENCES actors (id),
    operation      TEXT NOT NULL,
    entity_kind    TEXT NOT NULL,
    entity_id      TEXT NOT NULL,
    reason         TEXT NOT NULL CHECK (length(trim(reason)) > 0),
    payload_digest TEXT NOT NULL,
    at             TEXT NOT NULL
);

CREATE INDEX idx_audit_actor ON audit (actor_id);
CREATE INDEX idx_audit_at ON audit (at);
CREATE INDEX idx_audit_entity ON audit (entity_kind, entity_id);

-- The single ordered notification feed (FR-N1). seq IS the /events cursor;
-- there is deliberately no second sequence anywhere, so no client ever
-- merges orderings across the two stores.
CREATE TABLE events (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    kind        TEXT NOT NULL,
    entity_kind TEXT NOT NULL,
    entity_id   TEXT NOT NULL,
    actor_id    TEXT REFERENCES actors (id),
    audit_id    TEXT REFERENCES audit (id),
    summary     TEXT NOT NULL DEFAULT '{}',
    at          TEXT NOT NULL
);

CREATE INDEX idx_events_at ON events (at);
