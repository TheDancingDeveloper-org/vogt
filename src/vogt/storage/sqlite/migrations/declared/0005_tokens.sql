-- 0005_tokens — scoped credentials bound to actors (FR-S3).
--
-- Only the hash is stored. A token is shown once, at issue time, and cannot
-- be recovered afterwards — if the operator loses it, the answer is to
-- rotate, not to look it up. A table that can hand back its own credentials
-- is a table that leaks them.
--
-- Scopes are instance-wide in v1: an agent holding `work.write` can write to
-- every project. That is a known limitation rather than an oversight
-- (`REQUIREMENTS.md` §3), recorded here because this is the table somebody
-- will read when they wonder why per-project scopes do not work.

CREATE TABLE tokens (
    id           TEXT PRIMARY KEY NOT NULL,
    actor_id     TEXT NOT NULL REFERENCES actors (id),
    name         TEXT NOT NULL,
    token_hash   TEXT NOT NULL UNIQUE,
    scopes       TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    expires_at   TEXT,
    last_used_at TEXT,
    revoked_at   TEXT,
    revoked_reason TEXT
);

CREATE INDEX idx_tokens_actor ON tokens (actor_id);
CREATE INDEX idx_tokens_live ON tokens (revoked_at);

-- Both allow and deny decisions are audited (FR-S5). A denial is the more
-- interesting row: it is the one that tells you an agent tried something it
-- could not do, which is exactly what you want to see.
--
-- Kept separate from `audit` because an authorization decision is not a
-- declared write: it changes nothing, carries no reason from the caller, and
-- happens on reads too. Folding it into `audit` would make "every audit row
-- is a change" false, and that invariant is worth more than one table.
CREATE TABLE auth_decisions (
    id           TEXT PRIMARY KEY NOT NULL,
    at           TEXT NOT NULL,
    decision     TEXT NOT NULL CHECK (decision IN ('allow', 'deny')),
    reason_code  TEXT NOT NULL,
    operation    TEXT NOT NULL,
    scope        TEXT,
    actor_id     TEXT REFERENCES actors (id),
    token_id     TEXT REFERENCES tokens (id),
    identity_ref TEXT,
    transport    TEXT NOT NULL,
    detail       TEXT
);

CREATE INDEX idx_auth_at ON auth_decisions (at);
CREATE INDEX idx_auth_decision ON auth_decisions (decision, operation);
