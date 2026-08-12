-- 0001_foundation — the observed store's metadata only.
--
-- The evidence tables (`sweeps`, `observations`) and the derived `latest_*`
-- tables arrive at M2 with the collector framework that writes them
-- (`SCHEMA.md` §3). They are deliberately absent here rather than created
-- empty: a table nobody writes is a place for the schema and the design to
-- drift apart quietly.
--
-- This store exists at M0 so that the migration framework, the store
-- interface, backup boundaries and `status` all treat two stores as normal
-- from the first commit, not as an M2 refactor.

CREATE TABLE meta (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
