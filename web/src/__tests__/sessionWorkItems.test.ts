// The rail's work-item chip facts and the close warning (WI-998).

import { beforeEach, describe, expect, it, vi } from "vitest";

const getWorkMany = vi.hoisted(() => vi.fn());
vi.mock("../vogtApi", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../vogtApi")>()),
  getWorkMany,
}));

import { VogtBusy, VogtUnavailable, liveSessionsWarning, type SessionSummary } from "../vogtApi";
import {
  invalidateWorkItemFacts,
  watchSessionWorkItems,
  workItemFacts,
} from "../sessionWorkItems";

function rows(...items: Array<[string, string, string]>) {
  return {
    items: items.map(([ref, title, state]) => ({ ref, title, state, kind: "feature", priority: "p2" })),
    missing: [],
  };
}

/** Let the queued batch go out and its answer land. */
async function settle(): Promise<void> {
  for (let n = 0; n < 5; n += 1) await Promise.resolve();
}

describe("session work items", () => {
  beforeEach(() => {
    getWorkMany.mockReset();
    invalidateWorkItemFacts();
  });

  it("reads a ref's title and state once and caches it", async () => {
    getWorkMany.mockResolvedValue(rows(["WI-7", "terminal render bug", "done"]));
    expect(workItemFacts("WI-7")).toBeUndefined();
    await vi.waitFor(() =>
      expect(workItemFacts("WI-7")).toEqual({ title: "terminal render bug", state: "done" }),
    );
    expect(getWorkMany).toHaveBeenCalledTimes(1);
    expect(getWorkMany.mock.calls[0]?.[0]).toEqual(["WI-7"]);
  });

  it("says null for a ref the core does not have", async () => {
    getWorkMany.mockResolvedValue({ items: [], missing: ["WI-404"] });
    workItemFacts("WI-404");
    await vi.waitFor(() => expect(workItemFacts("WI-404")).toBeNull());
  });

  it("reads every chip of one render in one request (WI-1224)", async () => {
    getWorkMany.mockImplementation(async (refs: string[]) =>
      rows(...refs.map((ref): [string, string, string] => [ref, `title ${ref}`, "open"])),
    );
    const refs = Array.from({ length: 50 }, (_, n) => `WI-${n + 1}`);
    // A rail render: every chip asks, some twice.
    for (const ref of refs) workItemFacts(ref);
    for (const ref of refs.slice(0, 10)) workItemFacts(ref);
    await vi.waitFor(() => expect(workItemFacts("WI-50")).toEqual({ title: "title WI-50", state: "open" }));
    expect(getWorkMany).toHaveBeenCalledTimes(1);
    expect(getWorkMany.mock.calls[0]?.[0]).toEqual(refs);
  });

  it("asks in pages when a rail names more refs than one read takes", async () => {
    getWorkMany.mockResolvedValue({ items: [], missing: [] });
    for (let n = 1; n <= 450; n += 1) workItemFacts(`WI-${n}`);
    await settle();
    expect(getWorkMany.mock.calls.map((call) => (call[0] as string[]).length)).toEqual([
      200, 200, 50,
    ]);
  });

  it("invalidates only the refs an event names, and only their failures", async () => {
    getWorkMany.mockResolvedValue(rows(["WI-1", "one", "open"], ["WI-2", "two", "open"]));
    workItemFacts("WI-1");
    workItemFacts("WI-2");
    await vi.waitFor(() => expect(workItemFacts("WI-2")).toEqual({ title: "two", state: "open" }));
    // WI-3 fails to read and is then left alone.
    getWorkMany.mockRejectedValueOnce(new VogtUnavailable(502, "core gone"));
    workItemFacts("WI-3");
    workItemFacts("WI-4");
    await settle();
    expect(getWorkMany).toHaveBeenCalledTimes(2);

    invalidateWorkItemFacts(["WI-1", "WI-3"]);
    expect(workItemFacts("WI-2")).toEqual({ title: "two", state: "open" });
    expect(workItemFacts("WI-4")).toBeUndefined(); // still failed: not asked
    getWorkMany.mockResolvedValue(rows(["WI-1", "one, edited", "in_progress"], ["WI-3", "three", "open"]));
    expect(workItemFacts("WI-1")).toBeUndefined();
    expect(workItemFacts("WI-3")).toBeUndefined();
    await vi.waitFor(() =>
      expect(workItemFacts("WI-1")).toEqual({ title: "one, edited", state: "in_progress" }),
    );
    expect(getWorkMany).toHaveBeenCalledTimes(3);
    expect(getWorkMany.mock.calls[2]?.[0]).toEqual(["WI-1", "WI-3"]);
  });

  it("asks a ref again sooner after a busy door than after an outage", async () => {
    vi.useFakeTimers();
    try {
      getWorkMany.mockRejectedValueOnce(new VogtBusy("busy", 2_000));
      getWorkMany.mockRejectedValueOnce(new VogtUnavailable(502, "core gone"));
      workItemFacts("WI-8");
      workItemFacts("WI-9");
      await settle();
      expect(getWorkMany).toHaveBeenCalledTimes(1);
      vi.advanceTimersByTime(10_001);
      getWorkMany.mockResolvedValue(rows(["WI-8", "eight", "open"], ["WI-9", "nine", "open"]));
      workItemFacts("WI-8");
      await settle();
      expect(getWorkMany).toHaveBeenCalledTimes(2);
      expect(getWorkMany.mock.calls[1]?.[0]).toEqual(["WI-8"]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not cache a read that was in the air when its item changed", async () => {
    let answer: (value: unknown) => void = () => {};
    getWorkMany.mockImplementationOnce(() => new Promise((resolve) => (answer = resolve)));
    workItemFacts("WI-5");
    await settle();
    invalidateWorkItemFacts(["WI-5"]);
    getWorkMany.mockResolvedValue(rows(["WI-5", "after", "done"]));
    answer(rows(["WI-5", "before", "open"]));
    await vi.waitFor(() => expect(workItemFacts("WI-5")).toEqual({ title: "after", state: "done" }));
    expect(getWorkMany).toHaveBeenCalledTimes(2);
  });

  it("re-reads sessions on a bind event and drops facts on a work change", async () => {
    vi.useFakeTimers();
    let emit: (event: { kind: string; summary?: unknown }) => void = () => {};
    const refresh = vi.fn().mockResolvedValue(undefined);
    const stop = watchSessionWorkItems((listener) => {
      emit = listener;
      return () => {};
    }, refresh);
    emit({ kind: "session.work_bound" });
    emit({ kind: "session.work_unbound" });
    vi.advanceTimersByTime(300);
    expect(refresh).toHaveBeenCalledTimes(1);
    stop();
    vi.useRealTimers();
  });

  it("drops only the refs work events name, coalesced over the debounce", async () => {
    getWorkMany.mockResolvedValue(rows(["WI-1", "one", "open"], ["WI-2", "two", "open"], ["WI-3", "three", "open"]));
    for (const ref of ["WI-1", "WI-2", "WI-3"]) workItemFacts(ref);
    await vi.waitFor(() => expect(workItemFacts("WI-3")).toEqual({ title: "three", state: "open" }));

    vi.useFakeTimers();
    try {
      let emit: (event: { kind: string; summary?: unknown }) => void = () => {};
      const stop = watchSessionWorkItems((listener) => {
        emit = listener;
        return () => {};
      }, vi.fn().mockResolvedValue(undefined));
      emit({ kind: "work.updated", summary: { ref: "WI-1" } });
      emit({ kind: "work.transitioned", summary: { ref: "WI-3", from: "open", to: "done" } });
      vi.advanceTimersByTime(1_999);
      expect(workItemFacts("WI-1")).toEqual({ title: "one", state: "open" });
      vi.advanceTimersByTime(1);
      expect(workItemFacts("WI-1")).toBeUndefined();
      expect(workItemFacts("WI-2")).toEqual({ title: "two", state: "open" });
      expect(workItemFacts("WI-3")).toBeUndefined();

      // An event that names no ref still costs every chip, as before.
      emit({ kind: "work.updated" });
      vi.advanceTimersByTime(2_000);
      expect(workItemFacts("WI-2")).toBeUndefined();
      stop();
    } finally {
      vi.useRealTimers();
    }
  });

  it("warns about sessions still bound when an item finishes", () => {
    const live = [{ id: "ses_1" }, { id: "ses_2" }] as SessionSummary[];
    expect(liveSessionsWarning("WI-7", [])).toBeNull();
    expect(liveSessionsWarning("WI-7", undefined)).toBeNull();
    expect(liveSessionsWarning("WI-7", live.slice(0, 1))).toBe(
      "1 session is still bound to WI-7 (ses_1); it was not stopped or unbound.",
    );
    expect(liveSessionsWarning("WI-7", live)).toContain("2 sessions are still bound");
  });
});
