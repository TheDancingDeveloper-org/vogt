-- 0027_session_cold_wake — a session may opt out of cold-wake compaction.
--
-- When the deployment turns cold-wake compaction on (WI-2037), a wake of a
-- session whose prompt cache has expired and whose context is large starts a
-- fresh conversation seeded with a summary instead of resuming the
-- transcript. A session that must keep its full transcript opts out here:
-- 1 means "never compact on wake". Set at start, changeable at hibernate.
--
-- Numbered 0027: 0026 is WI-2036's watches. Additive, no backfill: every
-- existing session reads back 0, so it follows the deployment setting.

ALTER TABLE coding_sessions ADD COLUMN cold_wake_opt_out INTEGER NOT NULL DEFAULT 0;
