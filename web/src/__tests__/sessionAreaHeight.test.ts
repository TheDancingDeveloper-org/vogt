// The running-session block's height (WI-1664): a drag pins a height between
// about three rows and a caller-supplied maximum, double-click/reset returns
// the shipped auto height, and both survive a reload. A store that throws
// does not stop the resize.

import { beforeEach, describe, expect, it, vi } from "vitest";
import { createRoot } from "solid-js";
import {
  SESSION_AREA_DEFAULT_HEIGHT,
  SESSION_AREA_HEIGHT_KEY,
  SESSION_AREA_MIN_HEIGHT,
  createSessionAreaHeight,
} from "../sessionAreaHeight";

describe("createSessionAreaHeight", () => {
  beforeEach(() => localStorage.clear());

  it("starts at the shipped auto height", () => {
    createRoot((dispose) => {
      const area = createSessionAreaHeight();
      expect(area.height()).toBe(SESSION_AREA_DEFAULT_HEIGHT);
      dispose();
    });
  });

  it("clamps a chosen height and remembers it", () => {
    createRoot((dispose) => {
      const area = createSessionAreaHeight();
      area.setHeight(40, 600);
      expect(area.height()).toBe(SESSION_AREA_MIN_HEIGHT);
      area.setHeight(900, 480);
      expect(area.height()).toBe(480);
      dispose();
    });
    createRoot((dispose) => {
      expect(createSessionAreaHeight().height()).toBe(480);
      dispose();
    });
  });

  it("reset clears the stored height so the next session is auto again", () => {
    createRoot((dispose) => {
      const area = createSessionAreaHeight();
      area.setHeight(320, 600);
      area.reset();
      expect(area.height()).toBe(SESSION_AREA_DEFAULT_HEIGHT);
      dispose();
    });
    expect(localStorage.getItem(SESSION_AREA_HEIGHT_KEY)).toBeNull();
    createRoot((dispose) => {
      expect(createSessionAreaHeight().height()).toBe(SESSION_AREA_DEFAULT_HEIGHT);
      dispose();
    });
  });

  it("still resizes when localStorage throws", () => {
    const store = localStorage;
    vi.spyOn(store, "setItem").mockImplementation(() => {
      throw new Error("quota");
    });
    vi.spyOn(store, "getItem").mockImplementation(() => {
      throw new Error("denied");
    });
    createRoot((dispose) => {
      const area = createSessionAreaHeight();
      expect(area.height()).toBe(SESSION_AREA_DEFAULT_HEIGHT);
      area.setHeight(300, 600);
      expect(area.height()).toBe(300);
      dispose();
    });
  });
});
