import { describe, expect, it, vi } from "vitest";

import {
  CACHE_PERSIST_IDLE_TIMEOUT_MS,
  CACHE_PERSIST_INTERVAL_MS,
  cacheNeedsPersist,
  scheduleOnIdle,
  shouldScheduleCachePersist,
} from "../terminalCacheSchedule";

describe("terminal cache dirty check", () => {
  it("persists only when the byte cursor moved", () => {
    expect(cacheNeedsPersist(undefined, undefined)).toBe(false);
    expect(cacheNeedsPersist(10, undefined)).toBe(true);
    expect(cacheNeedsPersist(10, 10)).toBe(false);
    expect(cacheNeedsPersist(11, 10)).toBe(true);
  });

  it("does not reschedule a hidden pane that is already persisted", () => {
    expect(shouldScheduleCachePersist({
      outputPosition: 20,
      lastPersistedPosition: 20,
      hidden: true,
      alreadyScheduled: false,
    })).toBe(false);
    expect(shouldScheduleCachePersist({
      outputPosition: 21,
      lastPersistedPosition: 20,
      hidden: true,
      alreadyScheduled: false,
    })).toBe(false);
  });

  it("still schedules a visible pane whose cursor moved", () => {
    expect(shouldScheduleCachePersist({
      outputPosition: 21,
      lastPersistedPosition: 20,
      hidden: false,
      alreadyScheduled: false,
    })).toBe(true);
  });

  it("does not stack a second timer", () => {
    expect(shouldScheduleCachePersist({
      outputPosition: 21,
      lastPersistedPosition: 20,
      hidden: false,
      alreadyScheduled: true,
    })).toBe(false);
  });

  it("uses a 5s cadence constant", () => {
    expect(CACHE_PERSIST_INTERVAL_MS).toBe(5_000);
  });
});

describe("terminal cache idle scheduling", () => {
  it("runs on requestIdleCallback and cancels the timeout fallback", () => {
    const work = vi.fn();
    let idle: (() => void) | null = null;
    const clearTimeout = vi.fn();
    const handle = scheduleOnIdle(work, {
      requestIdleCallback: (cb) => {
        idle = cb;
        return 7;
      },
      cancelIdleCallback: vi.fn(),
      setTimeout: () => 3 as unknown as ReturnType<typeof setTimeout>,
      clearTimeout,
    }, CACHE_PERSIST_IDLE_TIMEOUT_MS);
    expect(work).not.toHaveBeenCalled();
    idle!();
    expect(work).toHaveBeenCalledTimes(1);
    expect(clearTimeout).toHaveBeenCalled();
    handle.cancel();
    idle!();
    expect(work).toHaveBeenCalledTimes(1);
  });

  it("falls back to the timeout when idle never fires", () => {
    const work = vi.fn();
    let timer: (() => void) | null = null;
    scheduleOnIdle(work, {
      setTimeout: (cb) => {
        timer = cb;
        return 1 as unknown as ReturnType<typeof setTimeout>;
      },
      clearTimeout: () => {},
    }, 50);
    timer!();
    expect(work).toHaveBeenCalledTimes(1);
  });

  it("cancel drops both the idle callback and the timeout", () => {
    const work = vi.fn();
    const cancelIdleCallback = vi.fn();
    const clearTimeout = vi.fn();
    const handle = scheduleOnIdle(work, {
      requestIdleCallback: () => 4,
      cancelIdleCallback,
      setTimeout: () => 9 as unknown as ReturnType<typeof setTimeout>,
      clearTimeout,
    });
    handle.cancel();
    expect(cancelIdleCallback).toHaveBeenCalledWith(4);
    expect(clearTimeout).toHaveBeenCalled();
    expect(work).not.toHaveBeenCalled();
  });
});
