-- 0013_upstream_truth — the work model converges on the forge (#181, design
-- #178 decisions 1, 2, 9, 10).
--
-- New-table / new-column DDL only, by decision (2026-08-21): the deployed
-- instance starts fresh at cutover — the declared store is wiped and every
-- repository re-imported through the #180 picker — so no existing work-item
-- row is transformed, re-keyed, or backfilled here. The migration still
-- applies cleanly in sequence on any database, because that is what the
-- migrator guarantees; it just never rewrites what it finds.

-- A project's link state is an explicit, persisted fact — set by `project
-- .import` (a successful clone + consolidate) or by `forge.link`, never
-- inferred per call from what tokens happen to be readable this second. On a
-- *linked* project the work items ARE the upstream issues: `work.create`
-- writes through to the forge and the subject key is the item's identity.
-- On an unlinked project the write verbs refuse with a typed error telling
-- the caller to link or publish (decision 10); the surface withdrawal that
-- follows from that is #183's, not this migration's.
ALTER TABLE projects ADD COLUMN link_state TEXT NOT NULL DEFAULT 'unlinked'
    CHECK (link_state IN ('unlinked', 'linked'));

-- The local overlay (decision 2 — invisible upstream). Keyed by the upstream
-- subject key (`gh:{owner}/{repo}#{n}`), NOT by a `wrk_*` id, because on a
-- linked project there is no `wrk_*` row: the observed mirror is the truth
-- for title/body/labels/open-closed, and this table carries only the
-- vogt-local semantics that must never cross the boundary — rank, a workflow
-- state richer than open/closed, and the priority/effort/assignee/initiative
-- fields decision 2 keeps invisible upstream. An overlay write is a declared
-- write like any other (audited, evented) and produces zero provider calls.
--
-- `rank` is schema for the vogt-local ordering; no operation sets it yet.
-- Relations stay in `work_relations` (declared-id keyed) and are not
-- re-keyed here; audit stays in the audit trail.
CREATE TABLE work_overlay (
    subject_key       TEXT PRIMARY KEY NOT NULL,
    project_id        TEXT NOT NULL REFERENCES projects (id),
    rank              REAL,
    workflow_state    TEXT,
    priority          TEXT CHECK (
        priority IS NULL OR priority IN ('p0', 'p1', 'p2', 'p3', 'p4')
    ),
    effort            TEXT CHECK (
        effort IS NULL OR effort IN ('xs', 's', 'm', 'l', 'xl')
    ),
    assignee_actor_id TEXT REFERENCES actors (id),
    initiative_id     TEXT REFERENCES initiatives (id),
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL
);

CREATE INDEX idx_work_overlay_project ON work_overlay (project_id);
