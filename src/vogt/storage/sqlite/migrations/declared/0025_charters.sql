-- 0025_charters — standing instructions per scope (WI-2038).
--
-- Append-only: each set writes the next version for its scope, so the history
-- is the rows and the current charter is the highest version. `scope_id` is ''
-- for the workspace, a project id or an initiative id. An empty body clears.
CREATE TABLE charters (
    scope_kind      TEXT NOT NULL
        CHECK (scope_kind IN ('workspace', 'project', 'initiative')),
    scope_id        TEXT NOT NULL,
    version         INTEGER NOT NULL CHECK (version >= 1),
    body            TEXT NOT NULL,
    author_actor_id TEXT NOT NULL REFERENCES actors (id),
    created_at      TEXT NOT NULL,
    reason          TEXT NOT NULL,
    PRIMARY KEY (scope_kind, scope_id, version)
);
