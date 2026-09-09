// Dormant-socket budget for terminal panes (F4, WI-128).
//
// A pane that is no longer active (another tab is showing, or it is the
// unfocused half of a split) used to *park*: close its WebSocket and, on
// return, reattach with `resume_from` and re-stream everything produced while
// away (bounded by F1, but still a re-stream). A *dormant* pane instead keeps
// its socket open and simply stops writing to xterm — it buffers output into
// its in-memory ring — so returning to it renders the buffered delta with no
// reattach and no re-stream.
//
// Open sockets are not free (each is a broadcast subscriber on the engine), so
// only the most-recently-active `MAX_DORMANT_SOCKETS` panes stay dormant; the
// rest fall back to parking and reattach via F1's bounded path. This module is
// the shared, per-document budget: pure book-keeping plus the eviction policy,
// with no reference to xterm or a socket, so it is unit-testable on its own.

/** How many panes may hold an open-but-idle socket at once, per document. */
export const MAX_DORMANT_SOCKETS = 4;

interface DormantEntry {
  /** Last time this pane was active; the eviction key (most-recent wins). */
  at: number;
  /** Close this pane's socket and fall back to parking. */
  evict: () => void;
}

const dormant = new Map<string, DormantEntry>();

/**
 * Ask to keep `id`'s socket open (dormant) instead of parking it. Returns true
 * if granted. When the budget is full, the least-recently-active dormant pane
 * is evicted (its `evict` runs) in favour of a newer one; a pane older than
 * every current dormant pane is denied and should park.
 *
 * Re-acquiring for a pane already dormant just refreshes its recency.
 */
export function acquireDormant(id: string, at: number, evict: () => void): boolean {
  const existing = dormant.get(id);
  if (existing) {
    existing.at = at;
    existing.evict = evict;
    return true;
  }
  if (dormant.size < MAX_DORMANT_SOCKETS) {
    dormant.set(id, { at, evict });
    return true;
  }
  // Full: evict the least-recently-active entry, but only for a newer pane.
  let lruId: string | null = null;
  let lruAt = Infinity;
  for (const [key, entry] of dormant) {
    if (entry.at < lruAt) {
      lruAt = entry.at;
      lruId = key;
    }
  }
  if (lruId === null || at <= lruAt) return false;
  const victim = dormant.get(lruId)!;
  dormant.delete(lruId);
  dormant.set(id, { at, evict });
  victim.evict();
  return true;
}

/** This pane is active again (or gone): give up its dormant slot. */
export function releaseDormant(id: string): void {
  dormant.delete(id);
}

/** Current number of dormant sockets (tests, diagnostics). */
export function dormantCount(): number {
  return dormant.size;
}

/** Forget all dormant slots. For tests between cases. */
export function resetDormancyForTest(): void {
  dormant.clear();
}

/** How a returning dormant pane should catch its xterm up. */
export type DormantResume =
  | { kind: "noop" }
  | { kind: "delta"; bytes: number }
  | { kind: "reset-tail" };

/**
 * Decide how to render what arrived while a pane was dormant.
 *
 * - `noop`: nothing arrived.
 * - `delta`: the buffered bytes fit the replay budget and are still retained in
 *   the ring, so write exactly them onto the frozen screen — no reset, no flash.
 * - `reset-tail`: too much was buffered (or the ring trimmed below it), so the
 *   screen is reset and a bounded ground-state tail replayed instead.
 *
 * `since` is `outputPosition - dormantStart`; `retained` is the current
 * in-memory ring length; `budget` is the per-pane replay cap.
 */
export function planDormantResume(
  since: number,
  retained: number,
  budget: number,
): DormantResume {
  if (since <= 0) return { kind: "noop" };
  if (since <= budget && since <= retained) return { kind: "delta", bytes: since };
  return { kind: "reset-tail" };
}
