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
  before = false,
): TerminalLayoutNode | null {
  if (node.type === "pane") {
    if (node.id !== targetPaneId) return null;
    return {
      type: "split",
      id: newSplitId(),
      direction,
      // `before` places the new pane ahead of the target — a drop on the
      // left/top edge lands the mirror there rather than always after.
      children: before ? [nextPane, node] : [node, nextPane],
    };
  }
  for (let index = 0; index < node.children.length; index += 1) {
    const child = node.children[index];
    if (!child) continue;
    const inserted = insertPane(child, targetPaneId, direction, nextPane, before);
    if (!inserted) continue;
    const children = node.children.slice();
    children[index] = inserted;
    return { ...node, children };
  }
  return null;
}

export interface DropOutcome {
  root: TerminalLayoutNode;
  activePaneId: string;
  /** True when a new pane was inserted; false when the session was already on
   *  screen and the existing pane was focused instead. */
  inserted: boolean;
}

/**
 * Drop an existing session onto a pane, splitting in the hit-tested direction.
 *
 * Duplicate guard (#355): a pane id is derived from its session id, so a
 * session can render at most once per workspace. If the dropped session is
 * already shown, this focuses that pane instead of inserting a second copy.
 * The session is MIRRORED — it is not moved or detached from anywhere else it
 * renders; the server's attach fans out from a snapshot, so each pane holds
 * its own WebSocket onto the same PTY.
 *
 * Returns null only when the target pane is gone (a race), matching
 * `insertPane`'s contract.
 */
export function dropSessionIntoPane(
  root: TerminalLayoutNode,
  targetPaneId: string,
  sessionId: string,
  direction: SplitDirection,
  before = false,
): DropOutcome | null {
  if (containsSession(root, sessionId)) {
    return { root, activePaneId: paneIdFor(sessionId), inserted: false };
  }
  const nextPane = makePane(sessionId);
  const next = insertPane(root, targetPaneId, direction, nextPane, before);
  if (!next) return null;
  return { root: next, activePaneId: nextPane.id, inserted: true };
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

/**
 * Rewrite each pane's session according to `remap`, leaving unmapped panes
 * untouched by reference so only the panes that changed re-render (and their
 * terminals re-attach). Panes carry a session-derived id, so a remapped pane
 * gets a fresh id and its `<Terminal>` remounts against the new session.
 */
function mapPaneSessions(
  node: TerminalLayoutNode,
  remap: Map<string, string>,
): TerminalLayoutNode {
  if (node.type === "pane") {
    const next = remap.get(node.sessionId);
    return next && next !== node.sessionId ? makePane(next) : node;
  }
  let changed = false;
  const children = node.children.map((child) => {
    const mapped = mapPaneSessions(child, remap);
    if (mapped !== child) changed = true;
    return mapped;
  });
  return changed ? { ...node, children } : node;
}

/**
 * Point a pane at a different session without changing the layout.
 *
 * If that session is already shown in another pane the two panes swap, so a
 * session is never duplicated across the tree. Returns the new root and the id
 * the retargeted pane now carries, or null when the target pane is gone.
 */
export function retargetPane(
  root: TerminalLayoutNode,
  targetPaneId: string,
  sessionId: string,
): { root: TerminalLayoutNode; activePaneId: string } | null {
  const target = findPane(root, targetPaneId);
  if (!target) return null;
  if (target.sessionId === sessionId) {
    return { root, activePaneId: target.id };
  }
  const remap = new Map<string, string>([[target.sessionId, sessionId]]);
  // The session is already on screen: swap, so the pane that held it takes on
  // the session the target used to show rather than vanishing.
  if (containsSession(root, sessionId)) {
    remap.set(sessionId, target.sessionId);
  }
  return {
    root: mapPaneSessions(root, remap),
    activePaneId: paneIdFor(sessionId),
  };
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
