/**
 * Arbitrating a one-finger touch over a terminal (#592).
 *
 * Three things want a vertical swipe over `.terminal-host`: the browser (a
 * native pan, which xterm 6 gives nothing to scroll), xterm's own gesture
 * scroller (a document-level listener with inertia, which moves the normal
 * buffer's scrollback) and, above both, the session pager on the phone
 * stage, which wants only horizontal moves. This module decides, per move,
 * two things the DOM handler in `Terminal.tsx` then acts on:
 *
 * - `claim`: call `preventDefault()` on this `touchmove`. Decided from the
 *   very first vertical-leaning move, not after an intent threshold — an
 *   Android WebView that sees one unprevented vertical move commits to a
 *   native pan and cancels the touch for everyone (suspect 1 in #592), so the
 *   threshold that made emulation look fine was the thing that broke the
 *   device. A horizontal-leaning move is left alone for the pager.
 * - `wheelLines`: whole lines to turn into synthetic wheel events. Only in
 *   the **alternate** buffer, where there is no scrollback for xterm's gesture
 *   to move and "back-scroll" means whatever a mouse wheel would mean to the
 *   TUI in charge — xterm turns a wheel there into a mouse report or, with
 *   DECSET 1007, into arrow keys. In the normal buffer this is 0: xterm's own
 *   scroller owns the swipe, and a second scroller doubling it is what the
 *   old handler was.
 */

/** Movement before the swipe's axis is fixed; below it the lean still claims. */
export const TERMINAL_TOUCH_INTENT_PX = 8;

export type TerminalTouchAxis = "pending" | "vertical" | "horizontal";
export type TerminalBufferType = "normal" | "alternate";

export interface TerminalTouchGesture {
  startX: number;
  startY: number;
  lastY: number;
  axis: TerminalTouchAxis;
  /** Fractional lines carried between moves so slow swipes still add up. */
  lineRemainder: number;
}

export interface TerminalTouchMove {
  gesture: TerminalTouchGesture;
  /** Prevent the browser's default for this move. */
  claim: boolean;
  /** Whole lines to emit as wheel events; sign follows wheel `deltaY`. */
  wheelLines: number;
}

export function beginTerminalTouch(x: number, y: number): TerminalTouchGesture {
  return { startX: x, startY: y, lastY: y, axis: "pending", lineRemainder: 0 };
}

export function moveTerminalTouch(
  gesture: TerminalTouchGesture,
  x: number,
  y: number,
  cellHeight: number,
  buffer: TerminalBufferType,
): TerminalTouchMove {
  const dx = x - gesture.startX;
  const dy = y - gesture.startY;
  const absX = Math.abs(dx);
  const absY = Math.abs(dy);
  let axis = gesture.axis;

  if (axis === "pending") {
    const leansVertical = absY >= absX;
    if (Math.max(absX, absY) < TERMINAL_TOUCH_INTENT_PX) {
      // Not yet a swipe — but a vertical lean is already claimed, so the
      // WebView never gets the one unprevented move it needs to take over.
      // `lastY` stays at the start so the distance travelled while the axis
      // was still open counts once it is fixed.
      return { gesture, claim: leansVertical, wheelLines: 0 };
    }
    axis = leansVertical ? "vertical" : "horizontal";
  }

  if (axis === "horizontal") {
    return { gesture: { ...gesture, axis, lastY: y }, claim: false, wheelLines: 0 };
  }

  const step = y - gesture.lastY;
  let lineRemainder = gesture.lineRemainder;
  let wheelLines = 0;
  if (buffer === "alternate") {
    // Finger up (negative step) reads as wheel-down: positive deltaY.
    lineRemainder += -step / Math.max(1, cellHeight);
    const whole = Math.trunc(lineRemainder);
    wheelLines = whole === 0 ? 0 : whole; // never -0
    lineRemainder -= whole;
  }
  return {
    gesture: { ...gesture, axis, lastY: y, lineRemainder },
    claim: true,
    wheelLines,
  };
}
