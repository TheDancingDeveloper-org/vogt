-- 0031_project_default_branch — the branch a project's source checks out by
-- default, recorded at import (WI-4026). Empty means "not recorded": a project
-- registered before this migration, or one with no origin HEAD to read. A
-- session then resolves the remote's HEAD and logs the choice, falling back to
-- `main` only as a last resort.
ALTER TABLE projects ADD COLUMN default_branch TEXT NOT NULL DEFAULT '';
