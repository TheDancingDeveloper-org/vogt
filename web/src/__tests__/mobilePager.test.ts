import { describe, expect, it } from "vitest";
import {
  adjacentMobilePagerIndex,
  beginMobilePagerGesture,
  moveMobilePagerGesture,
  settleMobilePagerIndex,
} from "../mobilePager";

describe("mobile terminal pager gesture", () => {
  it("waits for horizontal intent and leaves vertical motion unclaimed", () => {
    const start = beginMobilePagerGesture(100, 100);
    expect(moveMobilePagerGesture(start, 109, 100, 1, 3)).toMatchObject({
      offset: 0,
      claimed: false,
    });

    const vertical = moveMobilePagerGesture(start, 112, 124, 1, 3);
    expect(vertical).toMatchObject({ offset: 0, claimed: false });
    expect(vertical.gesture.axis).toBe("vertical");
    expect(
      moveMobilePagerGesture(vertical.gesture, 170, 125, 1, 3).claimed,
    ).toBe(false);
  });

  it("lets the session bar and dots claim an otherwise diagonal swipe", () => {
    const start = beginMobilePagerGesture(100, 100);
    const move = moveMobilePagerGesture(start, 112, 124, 1, 3, true);
    expect(move).toMatchObject({ offset: 12, claimed: true });
    expect(move.gesture.axis).toBe("horizontal");
  });

  it("tracks normally between sessions and resists only beyond the ends", () => {
    const start = beginMobilePagerGesture(100, 100);
    expect(moveMobilePagerGesture(start, 30, 100, 1, 3).offset).toBe(-70);
    expect(moveMobilePagerGesture(start, 170, 100, 0, 3).offset).toBe(21);
    expect(moveMobilePagerGesture(start, 30, 100, 2, 3).offset).toBe(-21);
  });

  it("settles at 60px only when an adjacent session exists", () => {
    expect(settleMobilePagerIndex(1, 3, -59)).toBeNull();
    expect(settleMobilePagerIndex(1, 3, -60)).toBe(2);
    expect(settleMobilePagerIndex(1, 3, 60)).toBe(0);
    expect(settleMobilePagerIndex(0, 3, 60)).toBeNull();
    expect(adjacentMobilePagerIndex(2, 3, -1)).toBeNull();
  });
});
