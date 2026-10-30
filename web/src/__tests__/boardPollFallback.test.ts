import { describe, expect, it } from "vitest";
import { boardPollDue } from "../Board";

// WI-2328: the board polled every interval on every tab even while the event
// stream was carrying each change. The poll is now the floor for a stream that
// died quietly, not a second copy of a healthy one.

const now = 1_770_000_000_000;
const base = {
  hidden: false,
  circuitOpen: false,
  pending: false,
  streamHealthy: true,
  loadedAt: now - 5_000,
  now,
  pollSeconds: 20,
};

describe("the board poll is a fallback for the stream", () => {
  it("skips a tick while the stream is healthy and the view is fresh", () => {
    expect(boardPollDue(base)).toBe(false);
    expect(boardPollDue({ ...base, loadedAt: now - 39_000 })).toBe(false);
  });

  it("reads once the view is two intervals old, before the badge calls it stale", () => {
    expect(boardPollDue({ ...base, loadedAt: now - 40_000 })).toBe(true);
  });

  it("polls every tick when the stream is not healthy", () => {
    expect(boardPollDue({ ...base, streamHealthy: false })).toBe(true);
  });

  it("polls when nothing has loaded yet", () => {
    expect(boardPollDue({ ...base, loadedAt: null })).toBe(true);
  });

  it("never polls hidden, behind an open breaker, or under a composed move", () => {
    const stale = { ...base, streamHealthy: false };
    expect(boardPollDue({ ...stale, hidden: true })).toBe(false);
    expect(boardPollDue({ ...stale, circuitOpen: true })).toBe(false);
    expect(boardPollDue({ ...stale, pending: true })).toBe(false);
  });

  it("counts reads over a minute of ticks: 1 healthy vs 3 without a stream", () => {
    const reads = (streamHealthy: boolean) => {
      let loadedAt = now;
      let count = 0;
      for (let t = 20_000; t <= 60_000; t += 20_000) {
        if (boardPollDue({ ...base, streamHealthy, loadedAt, now: now + t })) {
          count += 1;
          loadedAt = now + t;
        }
      }
      return count;
    };
    expect(reads(true)).toBe(1);
    expect(reads(false)).toBe(3);
  });
});
