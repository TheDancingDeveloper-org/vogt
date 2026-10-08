import { REPLAY_TAIL_MAX_BYTES, prepareReplayTail } from "./terminalReplay";

/**
 * How a retained terminal pane holds its socket (WI-128, F4).
 *
 * - `active`: the pane the user is looking at. Output is written to xterm.
 * - `dormant`: hidden (another tab, or the document hidden) but still within
 *   the per-document socket budget. The socket stays open and frames are
 *   appended to the in-memory ring, but nothing is written to xterm, so coming
 *   back replays only what arrived while it was hidden.
 * - `parked`: the socket is closed and the pane shows "Suspended". This is the
 *   fallback when more panes are hidden than the budget allows; resuming
 *   reattaches through the bounded attach path.
 *
 * A pane that is merely unfocused but still visible (the other half of a
 * split) is NOT dormant: it stays `active` and keeps rendering, so a split
 * never goes blank. Only a truly hidden pane leaves `active`.
 */
export type PaneMode = "active" | "dormant" | "parked";

/** Dormant sockets kept open per document. The rest fall back to parked. */
export const MAX_DORMANT_SOCKETS = 4;

export interface DormantCandidate {
  id: string;
  /** Epoch ms the pane was last the one being looked at. */
  lastActiveAt: number;
}

/**
 * Which hidden panes may keep a socket, most-recently-active first. Anything
 * past the budget is parked instead: its socket closes and it reattaches
 * through the bounded path on resume.
 */
export function dormantBudget(
  hidden: readonly DormantCandidate[],
  limit: number = MAX_DORMANT_SOCKETS,
): { dormant: string[]; parked: string[] } {
  const ranked = [...hidden].sort((a, b) => b.lastActiveAt - a.lastActiveAt);
  return {
    dormant: ranked.slice(0, Math.max(0, limit)).map((c) => c.id),
    parked: ranked.slice(Math.max(0, limit)).map((c) => c.id),
  };
}

/**
 * The mode a pane should be in.
 *
 * `hidden` is the only thing that takes a pane out of `active`: an unfocused
 * split half is visible, so it keeps rendering. Among hidden panes, the most
 * recently active stay dormant up to the budget and the overflow is parked.
 */
export function paneMode(opts: {
  hidden: boolean;
  id: string;
  hiddenPanes: readonly DormantCandidate[];
  limit?: number;
}): PaneMode {
  if (!opts.hidden) return "active";
  const { dormant } = dormantBudget(opts.hiddenPanes, opts.limit);
  return dormant.includes(opts.id) ? "dormant" : "parked";
}

export type DormantResume =
  | {
      kind: "delta";
      /** Bytes buffered since the pane went dormant, written as-is. */
      data: Uint8Array;
    }
  | {
      kind: "reset";
      /** Ground-state tail: the caller resets xterm before writing it. */
      data: Uint8Array;
    };

/**
 * How a dormant pane catches up when it becomes visible again, without a new
 * attach. A delta within the replay budget is byte-exact. Past it, the ring
 * is trimmed to a ground-state tail and the screen is reset, the same bound a
 * cold attach uses.
 */
export function planDormantResume(
  buffered: Uint8Array,
  ring: Uint8Array,
  outputPosition: number,
  budget: number = REPLAY_TAIL_MAX_BYTES,
): DormantResume {
  if (buffered.byteLength <= budget) {
    return { kind: "delta", data: buffered };
  }
  return { kind: "reset", data: prepareReplayTail(ring, outputPosition, budget).data };
}
