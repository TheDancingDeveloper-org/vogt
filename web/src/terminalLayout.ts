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
  /**
   * Fraction of the split's main axis each child occupies, one per child,
   * summing to 1 (#601). Absent means equal shares — old saved layouts and
   * freshly created splits render evenly until a divider is dragged.
   */
  sizes?: number[];
}

/** The smallest fraction of a split a pane may be dragged to (#601). Keeps a
 *  terminal above the width/height where xterm throws or `fitAndResize` bails. */
export const MIN_PANE_FRACTION = 0.08;

function equalSizes(count: number): number[] {
  return Array.from({ length: count }, () => 1 / count);
}

/**
 * A valid `sizes` array for `count` children: `count` positive fractions that
 * sum to 1. A missing, wrong-length, or malformed array (a structural edit
 * left it stale, or an old layout never had one) falls back to equal shares.
 */
export function normalizeSizes(count: number, sizes?: number[]): number[] {
  if (count <= 0) return [];
  if (
    !sizes ||
    sizes.length !== count ||
    sizes.some((s) => typeof s !== "number" || !Number.isFinite(s) || s <= 0)
  ) {
    return equalSizes(count);
  }
  const total = sizes.reduce((sum, s) => sum + s, 0);
  if (!(total > 0)) return equalSizes(count);
  return sizes.map((s) => s / total);
}

export type TerminalLayoutNode = PaneNode | SplitNode;

export interface SavedTerminalLayout {
  root: TerminalLayoutNode;
  activePaneId: string;
  broadcast?: boolean;
}

/**
 * The legacy session-derived pane id (#212). Panes no longer mint their id
 * this way — a pane's identity is now independent of the session it shows
 * (#600), so retargeting a pane keeps its id and only untouched panes are left
 * alone by the renderer. Kept for saved layouts written before #600, whose
 * stored ids happen to be `pane:<sessionId>`; `normalizeTerminalLayout`
 * preserves whatever id it finds, so those layouts still load. New code that
 * needs the pane showing a session must use `findPaneBySession`.
 */
export function paneIdFor(sessionId: string): string {
  return `pane:${sessionId}`;
}

function newPaneId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return `pane:${crypto.randomUUID()}`;
  }
  return `pane:${Date.now()}:${Math.random().toString(36).slice(2)}`;
}

export function makePane(sessionId: string): PaneNode {
  return { type: "pane", id: newPaneId(), sessionId };
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
      sizes: normalizeSizes(
        children.length,
        Array.isArray(obj.sizes) ? (obj.sizes as number[]) : undefined,
      ),
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

export function findSplit(
  node: TerminalLayoutNode,
  splitId: string,
): SplitNode | null {
  if (node.type === "pane") return null;
  if (node.id === splitId) return node;
  for (const child of node.children) {
    const found = findSplit(child, splitId);
    if (found) return found;
  }
  return null;
}

export function findPaneBySession(
  node: TerminalLayoutNode,
  sessionId: string,
): PaneNode | null {
  if (node.type === "pane") return node.sessionId === sessionId ? node : null;
  for (const child of node.children) {
    const found = findPaneBySession(child, sessionId);
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
  const shown = findPaneBySession(root, sessionId);
  if (shown) {
    return { root, activePaneId: shown.id, inserted: false };
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
  const sizes = normalizeSizes(node.children.length, node.sizes);
  const children: TerminalLayoutNode[] = [];
  const keptSizes: number[] = [];
  node.children.forEach((child, index) => {
    const next = removePane(child, targetPaneId);
    if (next !== child) changed = true;
    if (next) {
      children.push(next);
      keptSizes.push(sizes[index] ?? 0);
    }
  });
  if (children.length === 0) return null;
  if (children.length === 1) return children[0] ?? null;
  // Dropping a child renormalises the survivors' proportions (#601); a
  // descendant-only change keeps this split's sizes as they were.
  return changed
    ? { ...node, children, sizes: normalizeSizes(children.length, keptSizes) }
    : node;
}

/**
 * Rewrite each pane's session according to `remap`, leaving unmapped panes
 * untouched by reference so only the panes that changed re-render (and their
 * terminals re-attach). A remapped pane keeps its stable id (#600) and only
 * its `sessionId` changes, so the renderer touches exactly the retargeted pane
 * and every other pane keeps its xterm instance, scrollback and socket.
 */
function mapPaneSessions(
  node: TerminalLayoutNode,
  remap: Map<string, string>,
): TerminalLayoutNode {
  if (node.type === "pane") {
    const next = remap.get(node.sessionId);
    return next && next !== node.sessionId ? { ...node, sessionId: next } : node;
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
  // The target pane keeps its id — only the session it shows changes — so the
  // caller can focus it without deriving an id from the session (#600).
  return {
    root: mapPaneSessions(root, remap),
    activePaneId: target.id,
  };
}

/** Replace the split with the given id by `update(split)`, leaving every other
 *  node untouched by reference so only the edited split re-renders (#601). */
function mapSplit(
  node: TerminalLayoutNode,
  splitId: string,
  update: (split: SplitNode) => SplitNode,
): TerminalLayoutNode {
  if (node.type === "pane") return node;
  if (node.id === splitId) return update(node);
  let changed = false;
  const children = node.children.map((child) => {
    const mapped = mapSplit(child, splitId, update);
    if (mapped !== child) changed = true;
    return mapped;
  });
  return changed ? { ...node, children } : node;
}

/**
 * Move `delta` (a fraction of the main axis) from the child after `dividerIndex`
 * to the child before it, clamping both to `minFraction` so a pane can never be
 * dragged to zero (#601). Pure over the sizes array; the live drag applies it to
 * the sizes captured when the drag began, `resizeSplit` to the tree's current
 * sizes.
 */
export function applyDividerDelta(
  sizes: number[],
  dividerIndex: number,
  delta: number,
  minFraction: number = MIN_PANE_FRACTION,
): number[] {
  const i = dividerIndex;
  const j = dividerIndex + 1;
  if (i < 0 || j >= sizes.length) return sizes.slice();
  const pair = (sizes[i] ?? 0) + (sizes[j] ?? 0);
  // Both panes cannot both honour the minimum if the pair is too small to hold
  // two of them; clamp to what the pair allows.
  const min = Math.min(minFraction, pair / 2);
  let a = (sizes[i] ?? 0) + delta;
  if (a < min) a = min;
  if (a > pair - min) a = pair - min;
  const next = sizes.slice();
  next[i] = a;
  next[j] = pair - a;
  return next;
}

/**
 * Move `delta` across a divider in the split with `splitId`. Other children
 * keep their sizes. Pure: the only split whose object changes is the target.
 */
export function resizeSplit(
  root: TerminalLayoutNode,
  splitId: string,
  dividerIndex: number,
  delta: number,
  minFraction: number = MIN_PANE_FRACTION,
): TerminalLayoutNode {
  return mapSplit(root, splitId, (split) => ({
    ...split,
    sizes: applyDividerDelta(
      normalizeSizes(split.children.length, split.sizes),
      dividerIndex,
      delta,
      minFraction,
    ),
  }));
}

/** Commit an exact sizes array onto a split (the end of a drag) (#601). */
export function setSplitSizes(
  root: TerminalLayoutNode,
  splitId: string,
  sizes: number[],
): TerminalLayoutNode {
  return mapSplit(root, splitId, (split) => ({
    ...split,
    sizes: normalizeSizes(split.children.length, sizes),
  }));
}

/** Equalise the two children a divider separates (double-click / Home) (#601). */
export function resetDivider(
  root: TerminalLayoutNode,
  splitId: string,
  dividerIndex: number,
): TerminalLayoutNode {
  return mapSplit(root, splitId, (split) => {
    const i = dividerIndex;
    const j = dividerIndex + 1;
    const sizes = normalizeSizes(split.children.length, split.sizes);
    if (i < 0 || j >= sizes.length) return split;
    const avg = ((sizes[i] ?? 0) + (sizes[j] ?? 0)) / 2;
    const next = sizes.slice();
    next[i] = avg;
    next[j] = avg;
    return { ...split, sizes: next };
  });
}

export function pruneTerminalLayout(
  node: TerminalLayoutNode,
  sessionExists: (sessionId: string) => boolean,
): TerminalLayoutNode | null {
  if (node.type === "pane") {
    return sessionExists(node.sessionId) ? node : null;
  }
  let changed = false;
  const sizes = normalizeSizes(node.children.length, node.sizes);
  const children: TerminalLayoutNode[] = [];
  const keptSizes: number[] = [];
  node.children.forEach((child, index) => {
    const next = pruneTerminalLayout(child, sessionExists);
    if (next) {
      children.push(next);
      keptSizes.push(sizes[index] ?? 0);
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
