-- 0021_agent_policy — a project's agent policy is data on the project.
--
-- The policy is one JSON document rather than a column per fact: it is read
-- whole (into a brief, into a session's hooks) and written whole, and a fact
-- added later is a new key with a default, not another migration. A project
-- that states nothing stores the empty document, which reads back as no
-- policy.
ALTER TABLE projects ADD COLUMN agent_policy TEXT NOT NULL DEFAULT '{}';
