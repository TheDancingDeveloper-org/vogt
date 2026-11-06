-- 0030_project_mcp_extra — MCP servers a project's worker sessions get on
-- top of the worker set (WI-2746). A JSON list of names, like `exclusions`.
ALTER TABLE projects ADD COLUMN mcp_extra TEXT NOT NULL DEFAULT '[]';
