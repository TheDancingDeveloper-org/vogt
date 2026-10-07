// The rail's work-item chip facts and the close warning (WI-998).

import { beforeEach, describe, expect, it, vi } from "vitest";

const listWork = vi.hoisted(() => vi.fn());
vi.mock("../vogtApi", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../vogtApi")>()),
  listWork,
}));

import { liveSessionsWarning, type SessionSummary } from "../vogtApi";
import {
  invalidateWorkItemFacts,
  watchSessionWorkItems,
  workItemFacts,
} from "../sessionWorkItems";

describe("session work items", () => {
  beforeEach(() => {
    listWork.mockReset();
    invalidateWorkItemFacts();
  });

  it("reads a ref's title and state once and caches it", async () => {
    listWork.mockResolvedValue({
      items: [
        { ref: "WI-70", title: "not this one", state: "open" },
        { ref: "WI-7", title: "terminal render bug", state: "done" },
      ],
    });
    expect(workItemFacts("WI-7")).toBeUndefined();
    await vi.waitFor(() =>
      expect(workItemFacts("WI-7")).toEqual({ title: "terminal render bug", state: "done" }),
    );
    expect(listWork).toHaveBeenCalledTimes(1);
    expect(listWork.mock.calls[0]?.[0]).toMatchObject({ query: "WI-7", include_finished: true });
  });

  it("says null for a ref the core does not have", async () => {
    listWork.mockResolvedValue({ items: [] });
    workItemFacts("WI-404");
    await vi.waitFor(() => expect(workItemFacts("WI-404")).toBeNull());
  });

  it("re-reads sessions on a bind event and drops facts on a work change", async () => {
    vi.useFakeTimers();
    let emit: (event: { kind: string }) => void = () => {};
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
