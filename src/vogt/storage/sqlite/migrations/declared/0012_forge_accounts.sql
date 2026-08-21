-- 0012_forge_accounts — an actor's own forge identity, so upstream writes are
-- attributed to them rather than to the instance file token (#179, design #178
-- decision 4).
--
-- This table stores a token ENCRYPTED, and that is a deliberate inversion of
-- `0005_tokens`, which stores only a hash. A vogt-issued token never needs
-- recovery — a lost one is rotated, not looked up — so a hash is the safe
-- shape. A forge PAT is the opposite: Vogt must hand it back to the upstream
-- API to write as the actor, so it has to be recoverable, which means Fernet
-- ciphertext under `forge_account_key_file` and NEVER plaintext. The two
-- categories look alike and are not, so the distinction is recorded here where
-- someone will read it when they wonder why this table can decrypt its secret
-- and `tokens` cannot.
--
-- `login` and `scopes` are cleartext on purpose: a status read ("is my account
-- linked, and as whom?") must work with no key configured at all, and neither
-- is a secret. Only `encrypted_token` requires the key, and only the write
-- path ever reads it.
CREATE TABLE forge_accounts (
    actor_id        TEXT NOT NULL REFERENCES actors (id),
    host            TEXT NOT NULL,
    login           TEXT NOT NULL,
    scopes          TEXT NOT NULL,
    encrypted_token TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    PRIMARY KEY (actor_id, host)
);

CREATE INDEX idx_forge_accounts_actor ON forge_accounts (actor_id);
