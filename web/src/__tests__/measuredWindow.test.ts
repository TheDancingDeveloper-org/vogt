import { describe, expect, it } from "vitest";
import { MeasuredWindow } from "../measuredWindow";

describe("MeasuredWindow", () => {
  it("uses its estimate until content reports a size", () => {
    const windowed = new MeasuredWindow(120);
    windowed.setKeys(["a", "b", "c"]);

    expect(windowed.totalHeight()).toBe(360);
    expect(windowed.range(0, 120, 0)).toEqual({
      start: 0,
      end: 1,
      top: 0,
      total: 360,
    });
  });

  it("updates prefix offsets and total height from a measured card", () => {
    const windowed = new MeasuredWindow(120);
    windowed.setKeys(["a", "b", "c"]);

    expect(windowed.measure("b", 200)).toEqual({
      index: 1,
      top: 120,
      delta: 80,
    });
    expect(windowed.offsetOf(2)).toBe(320);
    expect(windowed.totalHeight()).toBe(440);
    expect(windowed.measure("b", 200)).toBeNull();
  });

  it("retains measurements by key when a window's list is refreshed", () => {
    const windowed = new MeasuredWindow(100);
    windowed.setKeys(["a", "b", "c"]);
    windowed.measure("b", 180);
    windowed.setKeys(["c", "b", "d"]);

    expect(windowed.offsetOf(1)).toBe(100);
    expect(windowed.offsetOf(2)).toBe(280);
    expect(windowed.totalHeight()).toBe(380);
  });

  it("finds the visible interval with variable heights", () => {
    const windowed = new MeasuredWindow(100);
    windowed.setKeys(["a", "b", "c", "d"]);
    windowed.measure("a", 80);
    windowed.measure("b", 180);

    expect(windowed.range(90, 100, 0)).toEqual({
      start: 1,
      end: 2,
      top: 80,
      total: 460,
    });
  });
});
