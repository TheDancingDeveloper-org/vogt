-- 0006_agent_activity — the agent activity index (`SCHEMA.md` §3.4).
--
-- What agents did, read from their own transcripts by the `agent-activity`
-- collector: one row per tool call, already redacted, plus the per-file
-- cursor that makes the read incremental. Both tables are a regenerable
-- index rather than evidence: dropping them and resetting the cursors
-- rebuilds them from the transcripts on disk, so they are mutable (a call's
-- row is completed when its result is read) and never referenced from the
-- declared store.

CREATE TABLE agent_activity_files (
    path             TEXT PRIMARY KEY NOT NULL,
    agent            TEXT NOT NULL,
    -- Byte offset at a line boundary: everything before it is indexed.
    byte_offset      INTEGER NOT NULL,
    size             INTEGER NOT NULL,
    agent_session_id TEXT,
    cwd              TEXT,
    updated_at       TEXT NOT NULL
);

CREATE TABLE agent_activity (
    id               TEXT PRIMARY KEY NOT NULL,
    sweep_id         TEXT NOT NULL,
    source_path      TEXT NOT NULL,
    call_id          TEXT NOT NULL,
    agent            TEXT NOT NULL,
    agent_session_id TEXT NOT NULL,
    cwd              TEXT,
    tool             TEXT NOT NULL,
    -- Redacted one-line summary of the call's input.
    summary          TEXT NOT NULL,
    -- Comma-delimited with leading and trailing commas (`,github,docker,`)
    -- so one tag is matched exactly with `LIKE '%,tag,%'`.
    services         TEXT NOT NULL DEFAULT ',',
    -- The call dumps configuration or environment: its result is never
    -- excerpted, even when it is read in a later sweep than the call.
    withheld         INTEGER NOT NULL DEFAULT 0 CHECK (withheld IN (0, 1)),
    error            INTEGER NOT NULL DEFAULT 0 CHECK (error IN (0, 1)),
    -- Redacted head-and-tail of an error result; null otherwise.
    excerpt          TEXT,
    at               TEXT NOT NULL,
    finished_at      TEXT
);

CREATE UNIQUE INDEX idx_agent_activity_call ON agent_activity (source_path, call_id);
CREATE INDEX idx_agent_activity_at ON agent_activity (at DESC, id DESC);
CREATE INDEX idx_agent_activity_session ON agent_activity (agent_session_id, at);
CREATE INDEX idx_agent_activity_errors ON agent_activity (error, at DESC);
