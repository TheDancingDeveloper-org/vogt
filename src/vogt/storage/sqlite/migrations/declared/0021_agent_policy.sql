-- 0021_agent_policy — a project stores its agent policy as one document.
--
-- Empty is no policy, which is what every project holds until one is stated.
-- The default matches AgentPolicy::default() rendered by python_json_dumps.
ALTER TABLE projects ADD COLUMN agent_policy TEXT NOT NULL
    DEFAULT '{"base_branch": "", "commit_identity": "", "deployed_setup": "", "forbidden_text": [], "landing": null, "main_branches": [], "merge_deploys_prod": false, "review": null, "rules": [], "session_wall_clock_minutes": null, "test_recipe": "", "verification": null}';
