-- 0019_session_grants — person-approved grants to a live session (WI-973).
--
-- A session (usually an overseer) asks for one scoped item for one target
-- session: a named credential, or (later) a capability. A person approves or
-- denies it in the Inbox. Approval is applied by the engine before this row
-- says `approved`, and the engine is the only thing that ever sees a secret
-- value: nothing here holds one. `expired` is not stored; it is an approved
-- row whose `expires_at` has passed, computed on read.
--
-- The target is the engine's session id, because a session started from the
-- GUI has no coding_sessions row and still needs to be grantable.
CREATE TABLE session_grants (
    id                       TEXT PRIMARY KEY,
    target_engine_session_id TEXT NOT NULL,
    kind                     TEXT NOT NULL CHECK (kind IN ('credential', 'capability')),
    var                      TEXT,
    project_id               TEXT,
    secret_name              TEXT,
    capability               TEXT,
    uses                     TEXT NOT NULL CHECK (uses IN ('once', 'ttl')),
    ttl_seconds              INTEGER NOT NULL CHECK (ttl_seconds BETWEEN 60 AND 86400),
    reason                   TEXT NOT NULL,
    requested_by             TEXT NOT NULL REFERENCES actors (id),
    requested_at             TEXT NOT NULL,
    state                    TEXT NOT NULL
        CHECK (state IN ('pending', 'approved', 'denied', 'revoked')),
    decided_by               TEXT REFERENCES actors (id),
    decided_at               TEXT,
    decision_reason          TEXT,
    expires_at               TEXT,
    revoked_by               TEXT REFERENCES actors (id),
    revoked_at               TEXT,
    CHECK (kind <> 'credential' OR (var IS NOT NULL AND project_id IS NOT NULL
                                    AND secret_name IS NOT NULL))
);

CREATE INDEX idx_session_grants_state ON session_grants (state, requested_at);
CREATE INDEX idx_session_grants_target ON session_grants (target_engine_session_id);
