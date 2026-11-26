// WI-4757: in-progress cards show a dot and a count of their live sessions,
// from the `live_sessions` on the board.list row. Only the in-progress column
// asks for it (`with_sessions` is a whole-call parameter, so that column's
// cells go in a call of their own); unknown is a muted `?`, never "none".

import { describe, expect, it } from "vitest";
import { waitFor } from "@solidjs/testing-library";
import Board from "../Board";
import { ATTENTION_RANK, rank } from "../Oversight";
import { fakeVogt, mountAt, workItem, type RecordedCall } from "./harness";

type Live = { count: number; worst: string | null; ids: string[] } | null;

const ITEMS: Record<string, { state: string; live: Live }> = {
  "WI-1": { state: "open", live: { count: 5, worst: "waiting", ids: [] } },
  "WI-2": { state: "in_progress", live: { count: 2, worst: "waiting", ids: ["a", "b"] } },
  "WI-3": { state: "in_progress", live: { count: 0, worst: null, ids: [] } },
  "WI-4": { state: "in_progress", live: null },
};

/** A server that, like the core, adds `live_sessions` to the rows of a call
 *  only when that call carries `with_sessions`. */
function boardServer(call: RecordedCall) {
  const body = call.body ?? {};
  const asked = body.with_sessions === true;
  const cells = (body.cells as { lane_key: string; state: string }[]).map((cell) => {
    const rows = Object.entries(ITEMS).filter(([, one]) => one.state === cell.state);
    return {
      lane_key: cell.lane_key,
      state: cell.state,
      items: rows.map(([ref, one]) =>
        workItem({ ref, state: one.state, ...(asked ? { live_sessions: one.live } : {}) }),
      ),
      total: rows.length,
      next_cursor: null,
    };
  });
  return {
    body: {
      cells,
      column_totals: { open: 1, in_progress: 3 },
      lane_totals: { "": 4 },
      total: 4,
      backlog_candidates: 4,
      declared_total: 4,
      snapshot: "s1",
      snapshot_at: "2026-08-17T10:01:00Z",
      revision: 1,
    },
  };
}

function mount() {
  const vogt = fakeVogt({ "POST /board/list": boardServer });
  const view = mountAt("/board", "/board", () => <Board />);
  return { vogt, container: view.container };
}

const live = (container: HTMLElement, ref: string) =>
  container.querySelector<HTMLElement>(`#board-card-${ref} .board-live`);

describe("live sessions on in-progress cards", () => {
  it("draws a dot and the count, named for assistive tech", async () => {
    const { container } = mount();
    await waitFor(() => expect(live(container, "WI-2")).toBeTruthy());
    const dot = live(container, "WI-2")!;
    expect(dot.textContent).toBe("2");
    expect(dot.getAttribute("aria-label")).toBe("2 live sessions, worst: waiting");
    expect(dot.className).toContain("board-live--waiting");
  });

  it("draws nothing for a count of zero", async () => {
    const { container } = mount();
    await waitFor(() => expect(live(container, "WI-2")).toBeTruthy());
    expect(container.querySelector("#board-card-WI-3")).toBeTruthy();
    expect(live(container, "WI-3")).toBeNull();
  });

  it("draws a muted ? for unknown, never 'no sessions'", async () => {
    const { container } = mount();
    await waitFor(() => expect(live(container, "WI-4")).toBeTruthy());
    const dot = live(container, "WI-4")!;
    expect(dot.textContent).toBe("?");
    expect(dot.getAttribute("aria-label")).toBe("live sessions unknown");
    expect(dot.className).toContain("board-live--unknown");
  });

  it("asks only for the in-progress column, and draws nothing elsewhere", async () => {
    const { vogt, container } = mount();
    await waitFor(() => expect(live(container, "WI-2")).toBeTruthy());
    const reads = vogt.matching("POST /board/list");
    const withSessions = reads.filter((read) => read.body?.with_sessions === true);
    expect(withSessions.length).toBeGreaterThan(0);
    for (const read of withSessions) {
      const states = (read.body?.cells as { state: string }[]).map((cell) => cell.state);
      expect(new Set(states)).toEqual(new Set(["in_progress"]));
    }
    for (const read of reads.filter((one) => one.body?.with_sessions !== true)) {
      const states = (read.body?.cells as { state: string }[]).map((cell) => cell.state);
      expect(states).not.toContain("in_progress");
    }
    expect(container.querySelector("#board-card-WI-1")).toBeTruthy();
    expect(live(container, "WI-1")).toBeNull();
  });
});

describe("the attention rank", () => {
  it("ranks done third, as the core's Attention::order_of does", () => {
    expect(ATTENTION_RANK.slice(0, 5)).toEqual([
      "approval",
      "blocked",
      "waiting",
      "done",
      "stalled",
    ]);
    expect(rank("done")).toBe(3);
    expect(rank("done")).toBeLessThan(rank("stalled"));
    expect(rank("done")).toBeGreaterThan(rank("waiting"));
  });
});
