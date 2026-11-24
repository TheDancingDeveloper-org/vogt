import { describe, expect, it } from "vitest";
import { recommendationCopy } from "../recommendation";

describe("recommendationCopy (WI-996 E0b)", () => {
  it("words a well-formed pick, naming the overseer that made it", () => {
    expect(recommendationCopy({ option: 2, label: "No", by: "oversight-1" })).toBe("oversight-1 recommends 2. No");
  });

  it("falls back to 'An overseer' when the record names no one", () => {
    expect(recommendationCopy({ option: "1", label: "Yes" })).toBe("An overseer recommends 1. Yes");
  });

  it("shows nothing for a missing or malformed pick", () => {
    expect(recommendationCopy(undefined)).toBeNull();
    expect(recommendationCopy(null)).toBeNull();
    expect(recommendationCopy("2")).toBeNull();
    expect(recommendationCopy({ option: 1 })).toBeNull();
    expect(recommendationCopy({ option: { x: 1 }, label: "No" })).toBeNull();
  });
});
