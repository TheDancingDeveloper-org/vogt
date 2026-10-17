// The starters reader pages Vogt's session list (railStarters.ts, WI-1736):
// nesting must work past the first page, not only for the first 50 sessions.

import { describe, expect, it, vi } from "vitest";
import { readAllStarters, STARTERS_MAX_PAGES, STARTERS_PAGE_SIZE } from "../railStarters";

const link = (child: string, starter: string | null) =>
  ({ id: `ses_${child}`, engine_session_id: child, started_by_session: starter }) as never;

describe("readAllStarters", () => {
  it("reads every page the engine says is truncated", async () => {
    const pages = [
      { sessions: [link("w1", "ovs")], truncated: true },
      { sessions: [link("w2", "ovs")], truncated: true },
      { sessions: [link("w3", "ovs")], truncated: false },
    ];
    const fetchPage = vi.fn(async (offset: number) => pages[offset === 0 ? 0 : offset === 1 ? 1 : 2]!);
    const starters = await readAllStarters(fetchPage);
    expect(Object.fromEntries(starters)).toEqual({ w1: "ovs", w2: "ovs", w3: "ovs" });
    expect(fetchPage.mock.calls.map((call) => call[0])).toEqual([0, 1, 2]);
  });

  it("stops at the page cap and at an empty page", async () => {
    const always = vi.fn(async () => ({ sessions: [link("w", "ovs")], truncated: true }));
    await readAllStarters(always);
    expect(always).toHaveBeenCalledTimes(STARTERS_MAX_PAGES);

    const empty = vi.fn(async () => ({ sessions: [], truncated: true }));
    await readAllStarters(empty);
    expect(empty).toHaveBeenCalledTimes(1);
  });

  it("asks for Vogt's full page size, not the default of 50", () => {
    expect(STARTERS_PAGE_SIZE).toBe(500);
  });
});
