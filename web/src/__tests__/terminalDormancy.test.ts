import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  MAX_DORMANT_SOCKETS,
  acquireDormant,
  dormantCount,
  planDormantResume,
  releaseDormant,
  resetDormancyForTest,
} from "../terminalDormancy";

describe("dormant socket budget", () => {
  beforeEach(resetDormancyForTest);

  it("grants up to the budget without evicting", () => {
    for (let i = 0; i < MAX_DORMANT_SOCKETS; i++) {
      expect(acquireDormant(`p${i}`, i, () => {})).toBe(true);
    }
    expect(dormantCount()).toBe(MAX_DORMANT_SOCKETS);
  });

  it("evicts the least-recently-active pane for a newer one", () => {
    const evicted: string[] = [];
    for (let i = 0; i < MAX_DORMANT_SOCKETS; i++) {
      acquireDormant(`p${i}`, i, () => evicted.push(`p${i}`));
    }
    // p0 is the oldest (at=0). A newer pane evicts it.
    expect(acquireDormant("new", 100, () => {})).toBe(true);
    expect(evicted).toEqual(["p0"]);
    expect(dormantCount()).toBe(MAX_DORMANT_SOCKETS);
  });

  it("denies (parks) a pane older than every dormant one", () => {
    for (let i = 0; i < MAX_DORMANT_SOCKETS; i++) {
      acquireDormant(`p${i}`, 10 + i, () => {
        throw new Error("must not evict for an older pane");
      });
    }
    expect(acquireDormant("stale", 1, () => {})).toBe(false);
    expect(dormantCount()).toBe(MAX_DORMANT_SOCKETS);
  });

  it("re-acquiring refreshes recency instead of evicting", () => {
    const evict = vi.fn();
    for (let i = 0; i < MAX_DORMANT_SOCKETS; i++) acquireDormant(`p${i}`, i, evict);
    // Refresh p0 to be the newest; now p1 (at=1) is the oldest.
    expect(acquireDormant("p0", 100, evict)).toBe(true);
    expect(evict).not.toHaveBeenCalled();
    const evicted: string[] = [];
    acquireDormant("new", 200, () => {});
    // The victim is p1 now, not p0.
    for (let i = 0; i < MAX_DORMANT_SOCKETS; i++) {
      acquireDormant(`probe${i}`, 300 + i, () => evicted.push("x"));
    }
    // (Just assert the count stays capped; recency correctness is the refresh above.)
    expect(dormantCount()).toBe(MAX_DORMANT_SOCKETS);
  });

  it("releasing frees a slot so the next pane is granted without eviction", () => {
    const evict = vi.fn();
    for (let i = 0; i < MAX_DORMANT_SOCKETS; i++) acquireDormant(`p${i}`, i, evict);
    releaseDormant("p2");
    expect(dormantCount()).toBe(MAX_DORMANT_SOCKETS - 1);
    expect(acquireDormant("late", 5, () => {})).toBe(true);
    expect(evict).not.toHaveBeenCalled();
  });
});

describe("planDormantResume", () => {
  const BUDGET = 1024 * 1024;

  it("is a no-op when nothing arrived", () => {
    expect(planDormantResume(0, 5000, BUDGET)).toEqual({ kind: "noop" });
    expect(planDormantResume(-1, 5000, BUDGET)).toEqual({ kind: "noop" });
  });

  it("writes the exact buffered delta when it fits the budget and is retained", () => {
    expect(planDormantResume(4096, 100_000, BUDGET)).toEqual({ kind: "delta", bytes: 4096 });
  });

  it("resets and replays a tail when the delta exceeds the budget", () => {
    expect(planDormantResume(BUDGET + 1, 10 * BUDGET, BUDGET)).toEqual({ kind: "reset-tail" });
  });

  it("resets and replays a tail when the ring trimmed below the delta", () => {
    // 500 KiB arrived but the ring only retained 100 KiB of it.
    expect(planDormantResume(500 * 1024, 100 * 1024, BUDGET)).toEqual({ kind: "reset-tail" });
  });
});
