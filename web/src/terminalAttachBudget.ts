/**
 * How many panes may open an attach socket on load (WI-1413).
 *
 * Every retained terminal tab mounts, and each one used to open its socket
 * immediately — even a pane that was about to park — so a long-lived tab with
 * dozens of sessions opened dozens of sockets, PTY readers and subscribers in
 * one tick. The budget keeps that to the panes the reader can actually see
 * plus a few recent ones, so opening a recent session is still instant and
 * everything else attaches the first time it is viewed.
 *
 * A pane that is not parked takes a slot unconditionally: the reader is
 * looking at it (or at its split half) and it must attach. Parked panes share
 * the remainder, most recently active first.
 */

/** Parked panes warmed on load, on top of whatever is currently visible. */
export const MAX_INITIAL_PARKED_ATTACHES = 3;

export interface AttachCandidate {
  id: string;
  /** Epoch ms the pane was last the one being looked at. */
  lastActiveAt: number;
  /** A visible pane. Always attached; it does not spend the parked budget. */
  visible: boolean;
}

/**
 * Which panes may attach now. Visible panes always may. Of the parked ones,
 * the most recently active may, up to `parkedLimit`; the rest wait until they
 * are viewed.
 */
export function initialAttachBudget(
  panes: readonly AttachCandidate[],
  parkedLimit: number = MAX_INITIAL_PARKED_ATTACHES,
): { attach: string[]; defer: string[] } {
  const visible = panes.filter((p) => p.visible).map((p) => p.id);
  const parked = panes
    .filter((p) => !p.visible)
    .sort((a, b) => b.lastActiveAt - a.lastActiveAt);
  const warm = parked.slice(0, Math.max(0, parkedLimit)).map((p) => p.id);
  const defer = parked.slice(Math.max(0, parkedLimit)).map((p) => p.id);
  return { attach: [...visible, ...warm], defer };
}
