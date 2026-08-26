import { afterEach, describe, expect, it, vi } from "vitest";
import { currentWake, noteForeground, onWake, reconcile, wakeLog } from "../wakeCoordinator";

afterEach(() => vi.useRealTimers());

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

  it("shares an in-flight resource read and records its result", async () => {
    vi.useFakeTimers();
    const stop = onWake(() => {});
    noteForeground("manual");
    await vi.advanceTimersByTimeAsync(250);
    const wake = currentWake()!;
    let release!: (value: string) => void;
    const pending = new Promise<string>((resolve) => { release = resolve; });
    const run = vi.fn(() => pending);
    const first = reconcile("sessions", wake, async () => run());
    const second = reconcile("sessions", wake, async () => run());
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
});
