// #592: back-scroll did not work in a terminal on the Android app. Three
// scrollers overlapped on a vertical swipe and the WebView's native pan won
// the first move and cancelled the rest. These pin the arbitration.
import { describe, expect, it } from "vitest";

import {
  beginTerminalTouch,
  moveTerminalTouch,
  TERMINAL_TOUCH_INTENT_PX,
  type TerminalTouchGesture,
} from "../terminalTouch";

const CELL = 20;

function swipe(
  steps: Array<[number, number]>,
  buffer: "normal" | "alternate" = "normal",
): { claims: boolean[]; wheel: number[]; gesture: TerminalTouchGesture } {
  let gesture = beginTerminalTouch(100, 300);
  const claims: boolean[] = [];
  const wheel: number[] = [];
  for (const [x, y] of steps) {
    const move = moveTerminalTouch(gesture, x, y, CELL, buffer);
    gesture = move.gesture;
    claims.push(move.claim);
    wheel.push(move.wheelLines);
  }
  return { claims, wheel, gesture };
}

describe("terminal touch arbitration", () => {
  it("claims a vertical-leaning move from the very first pixel", () => {
    // Below the intent threshold — the moves the old handler let through,
    // and the moves an Android WebView needs to start a native pan.
    const { claims, gesture } = swipe([[100, 302], [101, 305]]);
    expect(claims).toEqual([true, true]);
    expect(gesture.axis).toBe("pending");
  });

  it("leaves a horizontal-leaning move to the pager, before and after intent", () => {
    const { claims, gesture } = swipe([[104, 301], [120, 303], [160, 306]]);
    expect(claims).toEqual([false, false, false]);
    expect(gesture.axis).toBe("horizontal");
  });

  it("fixes the axis at the intent threshold and keeps it", () => {
    const { claims, gesture } = swipe([
      [100, 300 + TERMINAL_TOUCH_INTENT_PX],
      // A later horizontal wobble does not hand a vertical swipe to the pager.
      [130, 320],
    ]);
    expect(gesture.axis).toBe("vertical");
    expect(claims).toEqual([true, true]);
  });

  it("emits no wheel in the normal buffer — xterm's own scroller owns it", () => {
    const { wheel } = swipe([[100, 280], [100, 200], [100, 100]], "normal");
    expect(wheel).toEqual([0, 0, 0]);
  });

  it("turns an alternate-buffer swipe into whole wheel lines, finger-up = wheel-down", () => {
    // 100px up at 20px cells: five lines of wheel-down (positive deltaY).
    const { wheel } = swipe([[100, 290], [100, 200]], "alternate");
    expect(wheel).toEqual([0, 5]);
    // And back down: wheel-up.
    const back = swipe([[100, 310], [100, 400]], "alternate");
    expect(back.wheel).toEqual([0, -5]);
  });

  it("carries fractional lines across slow moves so they still add up", () => {
    // Twelve moves of 5px each = 60px = three lines, none of them a whole
    // line on its own.
    const steps: Array<[number, number]> = [];
    for (let i = 1; i <= 12; i++) steps.push([100, 300 - i * 5]);
    const { wheel } = swipe(steps, "alternate");
    expect(wheel.reduce((a, b) => a + b, 0)).toBe(3);
    expect(wheel.every((n) => Number.isInteger(n))).toBe(true);
  });

  it("does not divide by a zero cell height", () => {
    let gesture = beginTerminalTouch(0, 0);
    const move = moveTerminalTouch(gesture, 0, -40, 0, "alternate");
    expect(Number.isFinite(move.wheelLines)).toBe(true);
    gesture = move.gesture;
    expect(Number.isFinite(gesture.lineRemainder)).toBe(true);
  });
});
