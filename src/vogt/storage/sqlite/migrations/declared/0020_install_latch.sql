-- 0020_install_latch — first-run install mode is one-way (#903).
--
-- Install mode used to close the moment *any* token row existed. It now
-- stays open until a person holds a credential, so that the stack secret a
-- Docker quick start adopts at `init` (an agent-bound token) no longer hides
-- the first-run wizard. That change must not reopen the door on an instance
-- that is already running: one operated through the engine's break-glass
-- `ENGINE_TOKEN` — the old README quick start — holds only agent-bound tokens
-- and no person's, and would otherwise answer an unauthenticated admin
-- bootstrap after the upgrade.
--
-- So the closed state is latched, and the latch only ever gets set:
--
-- * at this migration, for a store that already held any token row — exactly
--   the rule it was closed under before. A fresh store migrates empty, before
--   `init` adopts anything, and stays open;
-- * by the declared store, in the same transaction that gives a person a
--   login or a token, by whatever path writes the row (the wizard, `vogt user
--   create`, `vogt token issue`, an instance merge). Removing that user later
--   does not reopen the door. (In code rather than as triggers: migrations
--   are plain DDL, see `split_statements`.)
CREATE TABLE install_latch (
    id        INTEGER PRIMARY KEY NOT NULL CHECK (id = 1),
    closed_at TEXT NOT NULL,
    reason    TEXT NOT NULL
);

INSERT INTO install_latch (id, closed_at, reason)
SELECT 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
       'upgrade: the store already held tokens'
WHERE EXISTS (SELECT 1 FROM tokens);
