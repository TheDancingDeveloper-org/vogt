// Whether a terminal's viewport is parked above its live tail.
//
// The jump-to-bottom chip appears exactly when this is true, so the whole
// feature turns on the direction of one comparison. It gets its own seam so a
// swapped operand, or an off-by-one where a viewport resting *at* the tail is
// wrongly called "behind", is caught here rather than as a chip that never
// hides or never shows.

/** The two positions the predicate needs, as xterm's `IBuffer` reports them:
 *  `baseY` is the first row of the live screen within the whole buffer, and
 *  `viewportY` the first row currently shown. */
export interface TerminalViewportPosition {
  viewportY: number;
  baseY: number;
}

/** True when the shown viewport sits above the live tail — there is output
 *  below the fold the reader has scrolled past. Equal positions mean the tail
 *  is already in view, which is not "behind". */
export function isBehindLiveTail(position: TerminalViewportPosition): boolean {
  return position.viewportY < position.baseY;
}
