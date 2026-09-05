import { describe, expect, it } from "vitest";
import { keyboardInsetFor } from "../viewportInsets";

// The phone's bottom bar and the terminal dock sit on `--keyboard-inset`.
// A reader who pinch-zooms must not get a phantom keyboard (WI-79).
describe("keyboard inset from the visual viewport", () => {
  it("is zero with no keyboard at scale 1", () => {
    expect(keyboardInsetFor(915, { height: 915, scale: 1 })).toBe(0);
  });
  it("is the keyboard's height at scale 1", () => {
    expect(keyboardInsetFor(915, { height: 555, scale: 1 })).toBe(360);
  });
  it("is zero when the reader has pinch-zoomed and no keyboard is up", () => {
    // 1.3× zoom crops the visual viewport to 704 CSS px of a 915 px layout.
    expect(keyboardInsetFor(915, { height: 704, scale: 1.3 })).toBe(0);
    // The 6% zoom that left a ~55px band under the composer on the phone.
    expect(keyboardInsetFor(890, { height: 837, scale: 1.063 })).toBe(0);
  });
  it("still finds the keyboard while zoomed", () => {
    // Keyboard takes 360 of 915 layout px; the zoomed viewport covers the rest.
    expect(keyboardInsetFor(915, { height: 555 / 1.3, scale: 1.3 })).toBe(360);
  });
  it("falls back to zero without a visual viewport", () => {
    expect(keyboardInsetFor(915, null)).toBe(0);
  });
});
