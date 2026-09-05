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
  /** Fractions of the split's main axis, one per child, summing to 1 (#601).
   *  Absent on legacy layouts and freshly created splits; treated as equal
   *  shares by `normalizeSizes`. */
  sizes?: number[];
}

/** Smallest share a divider drag will leave a neighbour, so a terminal can
 *  never be dragged to nothing (xterm throws / `fitAndResize` bails at 0). */
export const MIN_SPLIT_FRACTION = 0.05;

/**
 * Resolve a split's child fractions.
 *
 * Missing, wrong-length or non-positive input falls back to equal shares; a
 * valid but unnormalised array (what removing a child leaves behind) is
 * rescaled to sum to 1, preserving the surviving children's proportions.
 */
export function normalizeSizes(childCount: number, sizes?: number[]): number[] {
  if (childCount <= 0) return [];
  const equal = Array.from({ length: childCount }, () => 1 / childCount);
  if (!sizes || sizes.length !== childCount) return equal;
  if (!sizes.every((s) => Number.isFinite(s) && s > 0)) return equal;
  const total = sizes.reduce((sum, s) => sum + s, 0);
  if (total <= 0) return equal;
  return sizes.map((s) => s / total);
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
    const rawSizes =
      Array.isArray(obj.sizes) && obj.sizes.every((s) => typeof s === "number")
        ? (obj.sizes as number[])
        : undefined;
    return {
      type: "split",
      id: obj.id,
      direction: obj.direction,
      children,
      sizes: normalizeSizes(children.length, rawSizes),
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
      sizes: [0.5, 0.5],
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
  const oldSizes = normalizeSizes(node.children.length, node.sizes);
  let changed = false;
  const children: TerminalLayoutNode[] = [];
  const keptSizes: number[] = [];
  node.children.forEach((child, index) => {
    const next = removePane(child, targetPaneId);
    if (next !== child) changed = true;
    if (next) {
      children.push(next);
      keptSizes.push(oldSizes[index] ?? 0);
    }
  });
  if (children.length === 0) return null;
  if (children.length === 1) return children[0] ?? null;
  return changed
    ? { ...node, children, sizes: normalizeSizes(children.length, keptSizes) }
    : node;
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

/**
 * Rewrite one split's `sizes`, rebuilding only the spine down to it and
 * leaving every other node — panes included — referentially identical, so the
 * `<For>` that renders the tree never remounts a pane while a divider is
 * dragged (#601, and the identity contract from #600).
 */
function updateSplitSizes(
  node: TerminalLayoutNode,
  splitId: string,
  update: (sizes: number[]) => number[] | null,
): TerminalLayoutNode {
  if (node.type === "pane") return node;
  if (node.id === splitId) {
    const next = update(normalizeSizes(node.children.length, node.sizes));
    return next ? { ...node, sizes: next } : node;
  }
  let changed = false;
  const children = node.children.map((child) => {
    const mapped = updateSplitSizes(child, splitId, update);
    if (mapped !== child) changed = true;
    return mapped;
  });
  return changed ? { ...node, children } : node;
}

/**
 * Move `delta` (a fraction of the split's main axis) from the child after a
 * divider to the child before it, clamping both to `minFraction`. Neighbours
 * only — the rest of the split keeps its proportions.
 */
export function resizeSplit(
  root: TerminalLayoutNode,
  splitId: string,
  dividerIndex: number,
  delta: number,
  minFraction: number = MIN_SPLIT_FRACTION,
): TerminalLayoutNode {
  return updateSplitSizes(root, splitId, (sizes) => {
    const i = dividerIndex;
    const j = i + 1;
    if (i < 0 || j >= sizes.length) return null;
    const first = sizes[i] ?? 0;
    const second = sizes[j] ?? 0;
    const pair = first + second;
    const lo = Math.min(minFraction, pair / 2);
    const hi = pair - lo;
    const nextI = Math.min(hi, Math.max(lo, first + delta));
    const out = sizes.slice();
    out[i] = nextI;
    out[j] = pair - nextI;
    return out;
  });
}

/** Equalise the two neighbours of a divider (double-click / Home/End). */
export function resetDivider(
  root: TerminalLayoutNode,
  splitId: string,
  dividerIndex: number,
): TerminalLayoutNode {
  return updateSplitSizes(root, splitId, (sizes) => {
    const i = dividerIndex;
    const j = i + 1;
    if (i < 0 || j >= sizes.length) return null;
    const pair = (sizes[i] ?? 0) + (sizes[j] ?? 0);
    const out = sizes.slice();
    out[i] = pair / 2;
    out[j] = pair / 2;
    return out;
  });
}

export function pruneTerminalLayout(
  node: TerminalLayoutNode,
  sessionExists: (sessionId: string) => boolean,
): TerminalLayoutNode | null {
  if (node.type === "pane") {
    return sessionExists(node.sessionId) ? node : null;
  }
  const oldSizes = normalizeSizes(node.children.length, node.sizes);
  let changed = false;
  const children: TerminalLayoutNode[] = [];
  const keptSizes: number[] = [];
  node.children.forEach((child, index) => {
    const next = pruneTerminalLayout(child, sessionExists);
    if (next) {
      children.push(next);
      keptSizes.push(oldSizes[index] ?? 0);
    }
    if (next !== child) changed = true;
  });
  if (children.length === 0) return null;
  if (children.length === 1) return children[0] ?? null;
  return changed
    ? { ...node, children, sizes: normalizeSizes(children.length, keptSizes) }
    : node;
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
