export interface RegisteredProjectRepo {
  slug: string;
  name: string;
  root_path: string;
}

export interface GitRepositoryChoice extends RegisteredProjectRepo {
  /** Workspace-relative engine path. `.` names the workspace root itself. */
  repo: string | null;
  unavailableReason: string | null;
}

function normalizeAbsolutePath(path: string): string | null {
  const trimmed = path.trim();
  if (!trimmed.startsWith("/")) return null;
  const parts: string[] = [];
  for (const part of trimmed.split("/")) {
    if (!part || part === ".") continue;
    if (part === "..") parts.pop();
    else parts.push(part);
  }
  return `/${parts.join("/")}`;
}

function relativeToWorkspace(root: string, workspace: string): string | null {
  if (root === workspace) return ".";
  const prefix = workspace === "/" ? "/" : `${workspace}/`;
  return root.startsWith(prefix) ? root.slice(prefix.length) : null;
}

/** Map the declared project list onto the engine's explicit workspace scope. */
export function repositoryChoices(
  projects: readonly RegisteredProjectRepo[],
  workspaceRoot: string,
): GitRepositoryChoice[] {
  const workspace = normalizeAbsolutePath(workspaceRoot);
  return projects.map((project) => {
    const root = normalizeAbsolutePath(project.root_path);
    if (!root || !workspace) {
      return {
        ...project,
        repo: null,
        unavailableReason: "Project or engine workspace path is unavailable",
      };
    }
    const repo = relativeToWorkspace(root, workspace);
    if (repo !== null) {
      return {
        ...project,
        repo,
        unavailableReason: null,
      };
    }
    return {
      ...project,
      repo: null,
      unavailableReason: "Outside this engine workspace",
    };
  });
}
