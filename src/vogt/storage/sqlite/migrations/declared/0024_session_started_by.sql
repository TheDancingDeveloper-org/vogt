-- 0024_session_started_by — what a session was started for, and by whom.
--
-- A start may name a dispatch purpose (WI-1323, design §2.4): the profile
-- that resolved its template, model, budget and the rest. The row also
-- records who started it — the starting session's engine id, and whether
-- the start was a person's or a session's in the oversight or worker role
-- at that moment. These are facts about the start, set once, read from the
-- caller's verified identity and never from its parameters; the verdict
-- rule (WI-1319) and landing (WI-1328) check reviewer provenance on them.
--
-- Numbered 0024: 0023 is WI-1321's decision record. No backfill: every
-- session that already exists reads back with all three NULL.

ALTER TABLE coding_sessions ADD COLUMN purpose TEXT;
ALTER TABLE coding_sessions ADD COLUMN started_by_session TEXT;
ALTER TABLE coding_sessions ADD COLUMN started_by_kind TEXT;
