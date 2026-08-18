export type SplitDirection = "row" | "column";

export interface PaneNode {
  type: "pane";
  id: string;
  sessionId: string;
}

export interface SplitNode {
  type: "split";
  id: string;
  direction: SplitDirection;
  children: TerminalLayoutNode[];
}

export type TerminalLayoutNode = PaneNode | SplitNode;

export interface SavedTerminalLayout {
  root: TerminalLayoutNode;
  activePaneId: string;
  broadcast?: boolean;
}

export function paneIdFor(sessionId: string): string {
  return `pane:${sessionId}`;
}

export function makePane(sessionId: string): PaneNode {
  return { type: "pane", id: paneIdFor(sessionId), sessionId };
}

function newSplitId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return `split:${crypto.randomUUID()}`;
  }
  return `split:${Date.now()}:${Math.random().toString(36).slice(2)}`;
}

export function normalizeTerminalLayout(value: unknown): TerminalLayoutNode | null {
  if (!value || typeof value !== "object") return null;
  const obj = value as Record<string, unknown>;
  if (
    obj.type === "pane" &&
    typeof obj.id === "string" &&
    typeof obj.sessionId === "string"
  ) {
    return { type: "pane", id: obj.id, sessionId: obj.sessionId };
  }
  if (
    obj.type === "split" &&
    typeof obj.id === "string" &&
    (obj.direction === "row" || obj.direction === "column") &&
    Array.isArray(obj.children)
  ) {
    const children = obj.children
      .map((child) => normalizeTerminalLayout(child))
      .filter((child): child is TerminalLayoutNode => child !== null);
    if (children.length === 0) return null;
    if (children.length === 1) return children[0] ?? null;
    return {
      type: "split",
      id: obj.id,
      direction: obj.direction,
      children,
    };
  }
  return null;
}

export function containsSession(
  node: TerminalLayoutNode,
  sessionId: string,
): boolean {
  if (node.type === "pane") return node.sessionId === sessionId;
  return node.children.some((child) => containsSession(child, sessionId));
}

export function findPane(
  node: TerminalLayoutNode,
  paneId: string,
): PaneNode | null {
  if (node.type === "pane") return node.id === paneId ? node : null;
  for (const child of node.children) {
    const found = findPane(child, paneId);
    if (found) return found;
  }
  return null;
}

export function firstPane(node: TerminalLayoutNode): PaneNode | null {
  if (node.type === "pane") return node;
  for (const child of node.children) {
    const found = firstPane(child);
    if (found) return found;
  }
  return null;
}

export function collectPanes(node: TerminalLayoutNode): PaneNode[] {
  if (node.type === "pane") return [node];
  return node.children.flatMap((child) => collectPanes(child));
}

/**
 * Return a new tree only when the named pane was found.
 *
 * A missing target is not a harmless no-op: the caller may already have
 * created a PTY for the new pane and must roll that session back.
 */
export function insertPane(
  node: TerminalLayoutNode,
  targetPaneId: string,
  direction: SplitDirection,
  nextPane: PaneNode,
): TerminalLayoutNode | null {
  if (node.type === "pane") {
    if (node.id !== targetPaneId) return null;
    return {
      type: "split",
      id: newSplitId(),
      direction,
      children: [node, nextPane],
    };
  }
  for (let index = 0; index < node.children.length; index += 1) {
    const child = node.children[index];
    if (!child) continue;
    const inserted = insertPane(child, targetPaneId, direction, nextPane);
    if (!inserted) continue;
    const children = node.children.slice();
    children[index] = inserted;
    return { ...node, children };
  }
  return null;
}

export function removePane(
  node: TerminalLayoutNode,
  targetPaneId: string,
): TerminalLayoutNode | null {
  if (node.type === "pane") return node.id === targetPaneId ? null : node;
  let changed = false;
  const children: TerminalLayoutNode[] = [];
  for (const child of node.children) {
    const next = removePane(child, targetPaneId);
    if (next !== child) changed = true;
    if (next) children.push(next);
  }
  if (children.length === 0) return null;
  if (children.length === 1) return children[0] ?? null;
  return changed ? { ...node, children } : node;
}

export function pruneTerminalLayout(
  node: TerminalLayoutNode,
  sessionExists: (sessionId: string) => boolean,
): TerminalLayoutNode | null {
  if (node.type === "pane") {
    return sessionExists(node.sessionId) ? node : null;
  }
  let changed = false;
  const children: TerminalLayoutNode[] = [];
  for (const child of node.children) {
    const next = pruneTerminalLayout(child, sessionExists);
    if (next) children.push(next);
    if (next !== child) changed = true;
  }
  if (children.length === 0) return null;
  if (children.length === 1) return children[0] ?? null;
  return changed ? { ...node, children } : node;
}

/**
 * Commit a just-created PTY to the layout or delete it again.
 *
 * Keeping the transaction boundary here makes the failure path testable
 * without manufacturing a race in Solid's renderer.
 */
export async function commitCreatedPane(
  sessionId: string,
  commit: () => boolean,
  rollback: (sessionId: string) => Promise<void>,
): Promise<void> {
  try {
    if (!commit()) throw new Error("the target pane changed before insertion");
  } catch (error) {
    try {
      await rollback(sessionId);
    } catch (rollbackError) {
      const detail = rollbackError instanceof Error
        ? rollbackError.message
        : String(rollbackError);
      throw new Error(
        `${error instanceof Error ? error.message : String(error)}; created session ${sessionId} was retained because cleanup failed: ${detail}`,
      );
    }
    throw error;
  }
}
