-- 0007_sessions — the link from a work item or a project to a terminal the
-- engine is running for it (FR-E4, FR-E8).
--
-- This table is the *declared* link and nothing more: Vogt started a session,
-- for this project, for this item, attributed to this actor, with a reason a
-- person or an agent typed, and the write is audited like every other. It is
-- a decision, which is why it lives in the declared store beside
-- `suppressions` and `writeback_actions` rather than in the evidence store.
--
-- What is deliberately *not* here is the session's live state. Activity
-- (`idle` / `running` / `waiting-for-input` / `errored`), scrollback position
-- and exit code are the engine's to report over its own API and SSE stream
-- (FR-E2); a column caching any of them would be stale the instant it was
-- written, and a stale copy of somebody else's running state is worse than no
-- copy — it is a view that presents itself as current. `stopped_at` is the
-- one exception and is not an exception at all: it records that *Vogt*
-- stopped the session, not that the engine's process has ended.
--
-- Session outcomes — exit code, duration, working-tree delta — are FR-E6, and
-- are observations with freshness and trust like all other evidence. They
-- belong in `observed.sqlite3` with a `subject_key`, not in a column here.
-- The two questions are different: "what did we ask for, and why" is this
-- table; "what happened in there" is evidence, and evidence is re-collected.
--
-- `project_id` is NOT NULL and `work_item_id` is nullable because a session
-- always opens in some registered project's tree (FR-E3 makes the registry
-- the only source of that path) while only some sessions are opened *for* an
-- item.
CREATE TABLE coding_sessions (
    id                TEXT PRIMARY KEY NOT NULL,
    -- The engine's own id for the PTY. UNIQUE because every read from the
    -- engine's side — an SSE event, a session the operator killed — arrives
    -- carrying this and nothing else, and two rows claiming one terminal
    -- would make that lookup a guess.
    engine_session_id TEXT NOT NULL UNIQUE,
    project_id        TEXT NOT NULL REFERENCES projects (id),
    work_item_id      TEXT REFERENCES work_items (id),
    -- Whose writes the session's agent makes (FR-S10). Recorded here rather
    -- than inferred from the audit log later: the per-session token is
    -- revoked at session end, and the attribution has to survive that.
    actor_id          TEXT NOT NULL REFERENCES actors (id),
    -- The path the registry recorded, stored as it was used (FR-E3). Kept
    -- even though `projects.root_path` holds it today, because a project that
    -- later moves must not silently rewrite where a past session ran.
    cwd               TEXT NOT NULL,
    template          TEXT,
    reason            TEXT NOT NULL,
    started_at        TEXT NOT NULL,
    stopped_at        TEXT
);

-- The two list queries are "sessions for this project" and "sessions for this
-- work item", both newest-first and both usually filtered to the live ones.
CREATE INDEX idx_sessions_project ON coding_sessions (project_id, started_at);
CREATE INDEX idx_sessions_item ON coding_sessions (work_item_id, started_at);
CREATE INDEX idx_sessions_live ON coding_sessions (stopped_at);
