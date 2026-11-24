import { describe, expect, it, vi } from "vitest";

import {
  CACHE_PERSIST_IDLE_TIMEOUT_MS,
  CACHE_PERSIST_INTERVAL_MS,
  cacheNeedsPersist,
  installCacheFlushOnPageHide,
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

  it("keeps persisting a hidden pane whose cursor moved", () => {
    // No visibility input at all: hidden panes persist like visible ones.
    expect(shouldScheduleCachePersist({
      outputPosition: 20,
      lastPersistedPosition: 20,
      alreadyScheduled: false,
    })).toBe(false);
    expect(shouldScheduleCachePersist({
      outputPosition: 21,
      lastPersistedPosition: 20,
      alreadyScheduled: false,
    })).toBe(true);
    expect(shouldScheduleCachePersist({
      outputPosition: 40,
      lastPersistedPosition: 21,
      alreadyScheduled: false,
    })).toBe(true);
  });

  it("still schedules a visible pane whose cursor moved", () => {
    expect(shouldScheduleCachePersist({
      outputPosition: 21,
      lastPersistedPosition: 20,
      alreadyScheduled: false,
    })).toBe(true);
  });

  it("does not stack a second timer", () => {
    expect(shouldScheduleCachePersist({
      outputPosition: 21,
      lastPersistedPosition: 20,
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

describe("terminal cache flush on page hide", () => {
  function target(visibilityState = "visible") {
    const listeners = new Map<string, Set<() => void>>();
    return {
      visibilityState,
      addEventListener: (type: string, cb: () => void) => {
        if (!listeners.has(type)) listeners.set(type, new Set());
        listeners.get(type)!.add(cb);
      },
      removeEventListener: (type: string, cb: () => void) => {
        listeners.get(type)?.delete(cb);
      },
      fire: (type: string) => {
        for (const cb of listeners.get(type) ?? []) cb();
      },
    };
  }

  it("flushes on pagehide", () => {
    const flush = vi.fn();
    const win = target();
    const doc = target();
    installCacheFlushOnPageHide(flush, win, doc);
    win.fire("pagehide");
    expect(flush).toHaveBeenCalledTimes(1);
  });

  it("flushes on visibilitychange to hidden, not to visible", () => {
    const flush = vi.fn();
    const doc = target();
    installCacheFlushOnPageHide(flush, target(), doc);
    doc.fire("visibilitychange");
    expect(flush).not.toHaveBeenCalled();
    doc.visibilityState = "hidden";
    doc.fire("visibilitychange");
    expect(flush).toHaveBeenCalledTimes(1);
  });

  it("uninstall removes both listeners", () => {
    const flush = vi.fn();
    const win = target();
    const doc = target("hidden");
    const uninstall = installCacheFlushOnPageHide(flush, win, doc);
    uninstall();
    win.fire("pagehide");
    doc.fire("visibilitychange");
    expect(flush).not.toHaveBeenCalled();
  });
});
