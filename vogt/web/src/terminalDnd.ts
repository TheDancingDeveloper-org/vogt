import type { SplitDirection } from "./terminalLayout";

/**
 * Payload type for dragging a session (a session id) from the places rail into
 * the terminal workspace. A dedicated type keeps the workspace from reacting to
 * unrelated drags (the Board's card `text/plain` refs), while a `text/plain`
 * mirror is still set so a plain drop target can read the id.
 */
export const SESSION_DND_MIME = "application/x-vogt-session";

/** Which edge half of a pane the pointer is over. Drives both the split
 *  direction and the drop-zone highlight. */
export type DropZone = "left" | "right" | "top" | "bottom";

export interface DropRect {
  left: number;
  top: number;
  width: number;
  height: number;
}

/**
 * VS Code style edge hit-testing: pick the pane edge the pointer is closest to.
 * Left/right → a row (side-by-side) split; top/bottom → a column (stacked) one.
 */
export function dropZoneForPoint(
  rect: DropRect,
  clientX: number,
  clientY: number,
): DropZone {
  const relX = rect.width > 0 ? (clientX - rect.left) / rect.width : 0.5;
  const relY = rect.height > 0 ? (clientY - rect.top) / rect.height : 0.5;
  const distances: Array<[DropZone, number]> = [
    ["left", relX],
    ["right", 1 - relX],
    ["top", relY],
    ["bottom", 1 - relY],
  ];
  return distances.reduce((best, next) => (next[1] < best[1] ? next : best))[0];
}

export function directionForZone(zone: DropZone): SplitDirection {
  return zone === "left" || zone === "right" ? "row" : "column";
}

/** A drop on the left or top edge places the new pane ahead of the target. */
export function zoneInsertsBefore(zone: DropZone): boolean {
  return zone === "left" || zone === "top";
}

/** True when a drag carries a session payload this workspace should accept. */
export function dragCarriesSession(types: readonly string[] | undefined): boolean {
  return Boolean(types && types.includes(SESSION_DND_MIME));
}
