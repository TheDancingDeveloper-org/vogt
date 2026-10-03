// The engine's workspace root, for turning a tree path into the absolute path
// an agent session on the same host can open.
//
// Tree paths are relative to the workspace root, which only the engine knows;
// it reports it on `/api/status`. Asked once and remembered: the root is fixed
// for an engine's lifetime. A failed ask is forgotten so the next copy retries.

import { api } from "./api";

let rootRequest: Promise<string | null> | null = null;

function workspaceRoot(): Promise<string | null> {
  rootRequest ??= api
    .operationalStatus()
    .then((status) => status.storage.workspace_root || null)
    .catch(() => {
      rootRequest = null;
      return null;
    });
  return rootRequest;
}

/** Join a workspace-relative path onto an absolute root. */
export function joinWorkspacePath(root: string, path: string): string {
  const base = root.replace(/\/+$/, "");
  const rel = path.replace(/^\/+/, "");
  return rel ? `${base}/${rel}` : base;
}

/**
 * The absolute path of a workspace file on the engine host, or the
 * workspace-relative path when the root cannot be learned.
 */
export async function referencePath(path: string): Promise<string> {
  const root = await workspaceRoot();
  return root ? joinWorkspacePath(root, path) : path.replace(/^\/+/, "");
}

/** Test seam: forget the remembered root. */
export function resetWorkspaceRootForTests(): void {
  rootRequest = null;
}
