import { afterEach, describe, expect, it, vi } from "vitest";
import {
  cancelPendingWakeForTests,
  currentWake,
  FOLD_WINDOW_MS,
  noteForeground,
  onWake,
  reconcile,
  wakeLog,
} from "../wakeCoordinator";

afterEach(() => {
  cancelPendingWakeForTests();
  vi.useRealTimers();
});

function setVisibility(state: "visible" | "hidden") {
  Object.defineProperty(document, "visibilityState", { configurable: true, value: state });
  document.dispatchEvent(new Event("visibilitychange"));
}

describe("foreground wake coordinator", () => {
  it("coalesces a lifecycle burst into one numbered wake", async () => {
    vi.useFakeTimers();
    const wakes: string[] = [];
    const stop = onWake((wake) => wakes.push(`${wake.token}:${wake.reason}`));
    noteForeground("visibility");
    noteForeground("focus");
    noteForeground("resume");
    await vi.advanceTimersByTimeAsync(250);
    expect(wakes).toHaveLength(1);
    expect(wakes[0]).toMatch(/^[0-9]+:visibility$/);
    expect(currentWake()?.reason).toBe("visibility");
    stop();
  });

  // The event stream notes its own loss on this channel. That note is not the
  // app coming forward, so when a real lifecycle signal lands in the same
  // burst the wake must carry the lifecycle reason: listeners that only act
  // on a genuine return-to-front (the forced stream reconnect) key on it.
  it("lets a lifecycle signal outrank a stream-loss note in the same burst", async () => {
    vi.useFakeTimers();
    const wakes: string[] = [];
    const stop = onWake((wake) => wakes.push(wake.reason));
    noteForeground("sse-reconnect");
    noteForeground("focus");
    noteForeground("sse-reconnect");
    await vi.advanceTimersByTimeAsync(250);
    expect(wakes).toEqual(["focus"]);

    noteForeground("sse-reconnect");
    await vi.advanceTimersByTimeAsync(250);
    expect(wakes).toEqual(["focus", "sse-reconnect"]);
    stop();
  });

  it("shares an in-flight resource read and records its result", async () => {
    vi.useFakeTimers();
    const stop = onWake(() => {});
    noteForeground("manual");
    await vi.advanceTimersByTimeAsync(250);
    const wake = currentWake()!;
    let release!: (value: string) => void;
    const pending = new Promise<string>((resolve) => { release = resolve; });
    const run = vi.fn(() => pending);
    // The shared taxonomy test setup imports the production session store,
    // which also reconciles the real "sessions" resource on each wake. Keep
    // this unit test's synthetic resource separate from that listener.
    const first = reconcile("wake-test-sessions", wake, async () => run());
    const second = reconcile("wake-test-sessions", wake, async () => run());
    expect(second).toBe(first);
    expect(run).toHaveBeenCalledTimes(1);
    release("ready");
    await expect(first).resolves.toBe("ready");
    expect(wakeLog.at(-1)?.resources.at(-1)?.outcome).toBe("ok");
    stop();
  });

  it("does not emit while the document is hidden", async () => {
    vi.useFakeTimers();
    const original = document.visibilityState;
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "hidden" });
    const wakes: unknown[] = [];
    const stop = onWake((wake) => wakes.push(wake));
    noteForeground("visibility");
    await vi.advanceTimersByTimeAsync(250);
    expect(wakes).toHaveLength(0);
    Object.defineProperty(document, "visibilityState", { configurable: true, value: original });
    stop();
  });

  describe("folding a late lifecycle signal (Android resume → visibility → focus)", () => {
    afterEach(() => {
      Object.defineProperty(document, "visibilityState", { configurable: true, value: "visible" });
    });

    it("folds a focus that lands past the debounce into the same return", async () => {
      vi.useFakeTimers();
      const wakes: string[] = [];
      const stop = onWake((wake) => wakes.push(`${wake.reason}:${wake.hiddenMs > 0}`));
      setVisibility("hidden");
      await vi.advanceTimersByTimeAsync(10_000);
      // Android: resume and visibilitychange together, window focus ~600 ms on.
      noteForeground("resume");
      setVisibility("visible");
      await vi.advanceTimersByTimeAsync(600);
      noteForeground("focus");
      await vi.advanceTimersByTimeAsync(250);
      // The resume landed while still hidden, so the edge names the burst.
      expect(wakes).toEqual(["visibility:true"]);
      stop();
    });

    it("lets the visibility edge through when an early resume carried no hidden spell", async () => {
      vi.useFakeTimers();
      const wakes: string[] = [];
      const stop = onWake((wake) => wakes.push(wake.reason));
      setVisibility("hidden");
      await vi.advanceTimersByTimeAsync(5_000);
      Object.defineProperty(document, "visibilityState", { configurable: true, value: "visible" });
      noteForeground("resume");
      await vi.advanceTimersByTimeAsync(400);
      document.dispatchEvent(new Event("visibilitychange"));
      await vi.advanceTimersByTimeAsync(250);
      noteForeground("focus");
      await vi.advanceTimersByTimeAsync(250);
      expect(wakes).toEqual(["resume", "visibility"]);
      stop();
    });

    it("does not fold across a hidden spell or past the window", async () => {
      vi.useFakeTimers();
      const wakes: string[] = [];
      const stop = onWake((wake) => wakes.push(wake.reason));
      noteForeground("focus");
      await vi.advanceTimersByTimeAsync(250);
      setVisibility("hidden");
      setVisibility("visible");
      await vi.advanceTimersByTimeAsync(250);
      expect(wakes).toEqual(["focus", "visibility"]);
      await vi.advanceTimersByTimeAsync(FOLD_WINDOW_MS);
      noteForeground("focus");
      await vi.advanceTimersByTimeAsync(250);
      expect(wakes).toEqual(["focus", "visibility", "focus"]);
      stop();
    });

    it("never folds a manual wake or a stream-loss note", async () => {
      vi.useFakeTimers();
      const wakes: string[] = [];
      const stop = onWake((wake) => wakes.push(wake.reason));
      noteForeground("focus");
      await vi.advanceTimersByTimeAsync(250);
      noteForeground("manual");
      await vi.advanceTimersByTimeAsync(250);
      noteForeground("sse-reconnect");
      await vi.advanceTimersByTimeAsync(250);
      expect(wakes).toEqual(["focus", "manual", "sse-reconnect"]);
      stop();
    });
  });
});
