import { MAX_INITIAL_PARKED_ATTACHES } from "./terminalAttachBudget";

/**
 * The per-document gate on the first attach of a parked pane (WI-1413).
 *
 * `acquireInitialAttach` is the shared counter that enforces it, because each
 * pane mounts on its own and cannot see the others. Slots go in mount order:
 * the first parked panes to mount are the ones warmed. Visible panes never
 * ask — the reader is looking at them. A parked pane asks once, on load:
 * granted, it attaches so opening it later is instant; refused, it stays dark
 * until it is viewed.
 */
const held: string[] = [];

/**
 * Whether a parked pane may open its initial socket, and where it sits in the
 * stagger order. Returns the zero-based slot (stable for a given id) or `null`
 * when the budget is spent. Idempotent per id.
 */
export function acquireInitialAttach(id: string): number | null {
  const existing = held.indexOf(id);
  if (existing !== -1) return existing;
  if (held.length >= MAX_INITIAL_PARKED_ATTACHES) return null;
  held.push(id);
  return held.length - 1;
}

/** Test-only: a fresh document between cases. */
export function resetInitialAttaches(): void {
  held.length = 0;
}
