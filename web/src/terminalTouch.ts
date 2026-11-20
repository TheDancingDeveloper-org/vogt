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

export interface TerminalCell {
  col: number;
  row: number;
}

/**
 * Map a touch point to a grid cell. Width comes from the rendered screen and
 * height from the element that holds the rows: `.xterm-screen` is 0-high in
 * the phone layout, so a height read from it selects nothing. Clamped to grid.
 */
export function terminalCellAt(
  x: number,
  y: number,
  screen: { left: number; width: number },
  grid: { top: number; height: number },
  cols: number,
  rows: number,
): TerminalCell {
  const cellWidth = screen.width / Math.max(1, cols);
  const cellHeight = grid.height / Math.max(1, rows);
  const col = Math.floor((x - screen.left) / Math.max(1e-6, cellWidth));
  const row = Math.floor((y - grid.top) / Math.max(1e-6, cellHeight));
  return {
    col: Math.min(Math.max(col, 0), Math.max(0, cols - 1)),
    row: Math.min(Math.max(row, 0), Math.max(0, rows - 1)),
  };
}

const WORD_CHAR = /[\p{L}\p{N}_\-./~:@%+=]/u;

/**
 * The run of word characters around `col` in one line of text, as a start
 * column and length. Paths, URLs and identifiers select whole. Null when the
 * cell is blank or punctuation.
 */
export function wordSpanAt(
  line: string,
  col: number,
): { start: number; length: number } | null {
  if (col < 0 || col >= line.length || !WORD_CHAR.test(line[col] ?? "")) {
    return null;
  }
  let start = col;
  while (start > 0 && WORD_CHAR.test(line[start - 1] ?? "")) start -= 1;
  let end = col;
  while (end + 1 < line.length && WORD_CHAR.test(line[end + 1] ?? "")) end += 1;
  return { start, length: end - start + 1 };
}
