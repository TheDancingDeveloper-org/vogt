import { describe, expect, it } from "vitest";

import { isBehindLiveTail } from "../terminalScrollState";

describe("isBehindLiveTail — when the jump-to-bottom chip is offered", () => {
  it("is behind when the viewport is scrolled above the live tail", () => {
    // The reader dragged up through the scrollback: the shown top row is
    // earlier than the live screen's top row.
    expect(isBehindLiveTail({ viewportY: 40, baseY: 100 })).toBe(true);
  });

  it("is not behind when the viewport sits at the live tail", () => {
    // Caught up: the chip must hide, not linger one row from the bottom.
    expect(isBehindLiveTail({ viewportY: 100, baseY: 100 })).toBe(false);
  });

  it("is not behind for a fresh buffer with no scrollback", () => {
    expect(isBehindLiveTail({ viewportY: 0, baseY: 0 })).toBe(false);
  });

  it("stays behind while output keeps arriving above the parked viewport", () => {
    // New lines grow baseY without moving viewportY, so the gap widens and the
    // chip must remain — this is the case onScroll alone would miss.
    expect(isBehindLiveTail({ viewportY: 40, baseY: 250 })).toBe(true);
  });
});
