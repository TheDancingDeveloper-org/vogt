/**
 * One-finger terminal touch arbitration. Claim vertical movement immediately,
 * leave horizontal movement to the pager, and accumulate whole lines. The DOM
 * handler owns scrolling on both browser and native platforms and suppresses
 * xterm's competing synthetic gesture scroller. Mouse-tracking applications
 * receive wheels in either buffer; plain normal buffers move saved rows.
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
  /** Whole lines to move plain normal-buffer history; negative = older. */
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
  mouseTracking = false,
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

  const applicationWheel = buffer === "alternate" || mouseTracking;
  return {
    gesture: { ...gesture, axis, lastY: y, lineRemainder },
    claim: true,
    wheelLines: applicationWheel ? lines : 0,
    scrollLines: applicationWheel ? 0 : lines,
  };
}
