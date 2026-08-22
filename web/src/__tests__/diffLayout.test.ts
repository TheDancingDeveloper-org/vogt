import { describe, expect, it } from "vitest";
import {
  DIFF_SIDE_BY_SIDE_MIN_WIDTH,
  shouldRenderSideBySide,
} from "../diffLayout";

describe("diffLayout — responsive Monaco diff (#240)", () => {
  it("renders side-by-side at and above the threshold, inline below", () => {
    // The side-by-side diff needs two full code columns; below ~900px there is
    // not room for them, so the diff falls back to inline (unified).
    expect(shouldRenderSideBySide(DIFF_SIDE_BY_SIDE_MIN_WIDTH)).toBe(true);
    expect(shouldRenderSideBySide(DIFF_SIDE_BY_SIDE_MIN_WIDTH + 1)).toBe(true);
    expect(shouldRenderSideBySide(1280)).toBe(true);
    expect(shouldRenderSideBySide(DIFF_SIDE_BY_SIDE_MIN_WIDTH - 1)).toBe(false);
    expect(shouldRenderSideBySide(375)).toBe(false);
  });

  it("pins the threshold at 900px", () => {
    expect(DIFF_SIDE_BY_SIDE_MIN_WIDTH).toBe(900);
  });
});
