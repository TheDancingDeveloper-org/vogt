/**
 * One-finger terminal touch arbitration. Claim vertical movement once the
 * swipe's axis is decided (the host's `touch-action: none` already keeps the
 * browser from panning, and leaving the first pixels unclaimed lets a native
 * long-press start), leave horizontal movement to the pager, and accumulate
 * whole lines. The DOM
 * handler owns scrolling on both browser and native platforms and suppresses
 * xterm's competing synthetic gesture scroller. Mouse-tracking applications
 * receive wheels in either buffer; plain normal buffers move saved rows.
 */

/** Movement before the swipe's axis is fixed; below it nothing is claimed. */
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
  /** Timestamp (ms) of the last vertical move, for release velocity. */
  lastT?: number;
  /** Smoothed finger velocity in px/ms; positive = finger moving up. */
  velocity?: number;
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
  t?: number,
): TerminalTouchMove {
  const dx = x - gesture.startX;
  const dy = y - gesture.startY;
  const absX = Math.abs(dx);
  const absY = Math.abs(dy);
  let axis = gesture.axis;

  if (axis === "pending") {
    const leansVertical = absY >= absX;
  if (axis === "pending" && t !== undefined && gesture.lastT === undefined) {
    gesture = { ...gesture, lastT: t };
  }
    if (Math.max(absX, absY) < TERMINAL_TOUCH_INTENT_PX) {
      // Not yet a swipe: claim nothing, so a still finger can become a
      // native long-press. `lastY` stays at the start so the distance
      // travelled while the axis was still open counts once it is fixed.
      return { gesture, claim: false, wheelLines: 0, scrollLines: 0 };
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

  let velocity = gesture.velocity ?? 0;
  if (t !== undefined && gesture.lastT !== undefined && t > gesture.lastT) {
    const instant = -step / (t - gesture.lastT);
    velocity = 0.8 * instant + 0.2 * velocity;
  }

  const applicationWheel = buffer === "alternate" || mouseTracking;
  return {
    gesture: { ...gesture, axis, lastY: y, lineRemainder, lastT: t, velocity },
    claim: true,
    wheelLines: applicationWheel ? lines : 0,
    scrollLines: applicationWheel ? 0 : lines,
  };
}

/** Slowest release (px/ms) that still flings; slower lifts just stop. */
export const TERMINAL_FLING_MIN_VELOCITY = 0.3;
/** A finger held still this long (ms) before lifting does not fling. */
export const TERMINAL_FLING_MAX_IDLE_MS = 80;
/** Velocity kept per millisecond of fling (exponential friction). */
export const TERMINAL_FLING_DECAY_PER_MS = 0.996;

export interface TerminalFling {
  /** Lines per ms; positive = toward newer output (xterm `scrollLines`). */
  velocity: number;
  remainder: number;
}

/**
 * Momentum for a released vertical swipe, or null when it should just stop:
 * not a vertical swipe, too slow, or the finger rested before lifting.
 * Only the plain normal buffer flings — an alternate-screen or
 * mouse-tracking program gets wheel events, and a stream of synthetic
 * wheels after the finger lifts would drive it unasked.
 */
export function startTerminalFling(
  gesture: TerminalTouchGesture,
  now: number,
  cellHeight: number,
  buffer: TerminalBufferType,
  mouseTracking = false,
): TerminalFling | null {
  if (gesture.axis !== "vertical" || buffer !== "normal" || mouseTracking) return null;
  if (gesture.lastT === undefined || now - gesture.lastT > TERMINAL_FLING_MAX_IDLE_MS) return null;
  const v = gesture.velocity ?? 0;
  if (Math.abs(v) < TERMINAL_FLING_MIN_VELOCITY) return null;
  return { velocity: v / Math.max(1, cellHeight), remainder: gesture.lineRemainder };
}

/** Advance a fling by `dtMs`; returns whole lines to scroll, and null once it has stopped. */
export function stepTerminalFling(
  fling: TerminalFling,
  dtMs: number,
  cellHeight: number,
): { fling: TerminalFling | null; lines: number } {
  const decay = TERMINAL_FLING_DECAY_PER_MS ** dtMs;
  // Distance under exponential decay over dt: v * (1 - decay) / -ln(k).
  const travelled = (fling.velocity * (1 - decay)) / -Math.log(TERMINAL_FLING_DECAY_PER_MS);
  let remainder = fling.remainder + travelled;
  const whole = Math.trunc(remainder);
  remainder -= whole;
  const velocity = fling.velocity * decay;
  const stopped = Math.abs(velocity) * Math.max(1, cellHeight) < TERMINAL_FLING_MIN_VELOCITY / 6;
  return {
    fling: stopped ? null : { velocity, remainder },
    lines: whole === 0 ? 0 : whole,
  };
}
