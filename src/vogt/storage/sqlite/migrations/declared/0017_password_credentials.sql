-- 0017_password_credentials — a human signs in with a password, not a token.
--
-- One row per human actor that may log in. The password is stored as an
-- scrypt hash (`core/auth.py`), never recoverable; `scopes` is what every
-- session minted by a successful login carries, so widening or narrowing a
-- person is one row rather than a token per device.
--
-- `username` is the login handle and is deliberately its own column: the
-- actor's `identity_ref` (`human:<slug>`) is an audit identity, and tying a
-- login name to it would make renaming one a rewrite of the other.
CREATE TABLE password_credentials (
    actor_id      TEXT PRIMARY KEY NOT NULL REFERENCES actors (id),
    username      TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    scopes        TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);

-- Which kind of credential a token row is: `api` (minted by `token.issue`
-- or adopted at init), `session` (minted by a password login, expiring),
-- or `agent` (a coding session's own token). Existing rows are all `api`.
-- Listing and revoking "every browser session for this person" needs the
-- kind; nothing about authentication does — a session token is checked
-- exactly like any other.
ALTER TABLE tokens ADD COLUMN kind TEXT NOT NULL DEFAULT 'api';

CREATE INDEX idx_tokens_kind ON tokens (kind, revoked_at);
