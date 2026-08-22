// The resizable pane's width reset (#245). A rail dragged to a width the
// reader now regrets needs an escape hatch — double-click, or Home on the
// handle — that puts it back to the shipped default and persists that, clamped
// exactly as a drag would be.

import { describe, expect, it, beforeEach } from "vitest";
import { createRoot } from "solid-js";
import { createResizablePane } from "../resizablePane";

const OPTIONS = {
  key: "test-rail",
  defaultWidth: 248,
  min: 180,
  max: 420,
};

describe("createResizablePane reset", () => {
  beforeEach(() => localStorage.clear());

  it("returns the width to the shipped default", () => {
    createRoot((dispose) => {
      const pane = createResizablePane(OPTIONS);
      pane.setWidth(360);
      expect(pane.width()).toBe(360);

      pane.reset();
      expect(pane.width()).toBe(OPTIONS.defaultWidth);
      dispose();
    });
  });

  it("persists the reset width so the next session opens at the default", () => {
    createRoot((dispose) => {
      const pane = createResizablePane(OPTIONS);
      pane.setWidth(400);
      pane.reset();
      dispose();
    });

    // A fresh pane on the same key reads what reset persisted, not the 400 it
    // was dragged to before.
    createRoot((dispose) => {
      const reopened = createResizablePane(OPTIONS);
      expect(reopened.width()).toBe(OPTIONS.defaultWidth);
      dispose();
    });
  });

  it("clamps a default that falls outside the current bounds", () => {
    createRoot((dispose) => {
      const pane = createResizablePane({ ...OPTIONS, defaultWidth: 999 });
      pane.setWidth(200);
      pane.reset();
      expect(pane.width()).toBe(OPTIONS.max);
      dispose();
    });
  });
});
