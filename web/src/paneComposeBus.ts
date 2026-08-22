import type { SplitDirection } from "./terminalLayout";

/**
 * A live handle onto one terminal workspace (one terminal tab), registered by
 * `TerminalWorkspace` while it is mounted. It lets code outside the workspace —
 * the command palette and the rail's session menu — compose an existing session
 * into the *active* workspace without either owning its pane tree.
 *
 * `shownSessionIds` reads the workspace's reactive pane set, so a caller that
 * reads it inside a reactive scope tracks it and re-filters as panes change.
 */
export interface TerminalWorkspaceHandle {
  tabId: string;
  /** Session ids currently bound to a pane in this workspace. */
  shownSessionIds: () => string[];
  /** Split the active pane and show an existing session in the new one. */
  splitWithSession: (direction: SplitDirection, sessionId: string) => void;
  /** Re-target the active pane at an existing session (swaps on a clash). */
  showSessionInActivePane: (sessionId: string) => void;
}

const handles = new Map<string, TerminalWorkspaceHandle>();

export function registerTerminalWorkspace(
  handle: TerminalWorkspaceHandle,
): () => void {
  handles.set(handle.tabId, handle);
  return () => {
    if (handles.get(handle.tabId) === handle) handles.delete(handle.tabId);
  };
}

export function terminalWorkspaceHandle(
  tabId: string | null | undefined,
): TerminalWorkspaceHandle | undefined {
  return tabId ? handles.get(tabId) : undefined;
}
