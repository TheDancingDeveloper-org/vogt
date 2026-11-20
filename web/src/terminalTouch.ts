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


/** Hold this long, without moving, to select the word under the finger. */
export const TERMINAL_LONG_PRESS_MS = 500;

/** The DOM boxes a terminal cell lookup needs, in viewport coordinates. */
export interface TerminalCellBox {
  /** `term.element`, the container that spans the whole grid. */
  container: { left: number; top: number; width: number; height: number };
  /**
   * `.xterm-screen`, which reports the width without the scrollbar. It can
   * legitimately have height 0 on the phone layout, so vertical geometry must
   * come from the container.
   */
  screen?: { left: number; top: number; width: number; height: number } | null;
  /**
   * `.xterm-rows`: exactly `rows × cell height` high, with none of the
   * remainder FitAddon can leave below the grid. Preferred for the row.
   */
  rows?: { top: number; height: number } | null;
}

/**
 * The cell under a viewport point, or null when the point is outside the grid.
 * Width prefers the screen layer (the scrollbar makes the container wider);
 * the row prefers the rows layer's measured height, since a font-size estimate
 * is short of xterm's measured cell height and the container can include up
 * to one cell of FitAddon remainder. The screen layer, then the container, are
 * the fallbacks (the screen collapses to 0 height on some phone layouts).
 */
export function terminalCellAt(
  box: TerminalCellBox,
  cols: number,
  rows: number,
  x: number,
  y: number,
): { column: number; row: number } | null {
  if (cols <= 0 || rows <= 0) return null;
  const { container, screen, rows: rowsBox } = box;
  if (container.width <= 0 || container.height <= 0) return null;
  const left = screen && screen.width > 0 ? screen.left : container.left;
  const width = screen && screen.width > 0 ? screen.width : container.width;
  const grid =
    rowsBox && rowsBox.height > 0
      ? rowsBox
      : screen && screen.height > 0
        ? screen
        : container;
  const column = Math.floor(((x - left) / width) * cols);
  const row = Math.floor((y - grid.top) / (grid.height / rows));
  if (column < 0 || column >= cols || row < 0 || row >= rows) return null;
  return { column, row };
}
