/**
 * Arbitrating a one-finger touch over a terminal.
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
 *   native pan and cancels the touch for everyone, so the
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
  /**
   * Whole lines to move the normal-buffer scrollback directly, via
   * `term.scrollLines`; sign follows xterm's `scrollLines` (negative = older).
   * Non-zero only in the normal buffer. Desktop leaves this to xterm's own
   * gesture scroller and ignores it; the Capacitor WebView, where that scroller
   * is inert, applies it so a swipe actually moves the buffer (#592).
   */
  scrollLines: number;
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
      return { gesture, claim: leansVertical, wheelLines: 0, scrollLines: 0 };
    }
    axis = leansVertical ? "vertical" : "horizontal";
  }

  if (axis === "horizontal") {
    return {
      gesture: { ...gesture, axis, lastY: y },
      claim: false,
      wheelLines: 0,
      scrollLines: 0,
    };
  }

  // Accumulate the swipe's whole-line delta. Finger up (negative step) reads as
  // scrolling toward newer output: positive, matching wheel `deltaY` and
  // xterm's `scrollLines`. Fractional lines carry across moves so a slow drag
  // still adds up. Where the lines go depends on the buffer.
  const step = y - gesture.lastY;
  let lineRemainder = gesture.lineRemainder + -step / Math.max(1, cellHeight);
  const whole = Math.trunc(lineRemainder);
  const lines = whole === 0 ? 0 : whole; // never -0
  lineRemainder -= whole;

  // Alternate buffer: no scrollback, so a swipe means whatever a wheel means to
  // the TUI. Normal buffer: move the scrollback — desktop via xterm's own
  // gesture scroller (Terminal.tsx ignores `scrollLines` there), the Capacitor
  // WebView via `scrollLines` because that scroller is inert on the device.
  const alternate = buffer === "alternate";
  return {
    gesture: { ...gesture, axis, lastY: y, lineRemainder },
    claim: true,
    wheelLines: alternate ? lines : 0,
    scrollLines: alternate ? 0 : lines,
  };
}
