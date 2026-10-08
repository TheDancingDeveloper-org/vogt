import { describe, expect, it } from "vitest";
import { REPLAY_TAIL_MAX_BYTES } from "../terminalReplay";
import {
  MAX_DORMANT_SOCKETS,
  dormantBudget,
  paneMode,
  planDormantResume,
} from "../terminalDormancy";

const hidden = (ids: string[]) =>
  ids.map((id, i) => ({ id, lastActiveAt: i * 1000 }));

describe("dormant socket budget", () => {
  it("keeps the most recently active hidden panes dormant", () => {
    const { dormant, parked } = dormantBudget(hidden(["a", "b", "c", "d", "e"]));
    expect(dormant).toEqual(["e", "d", "c", "b"]);
    expect(parked).toEqual(["a"]);
    expect(MAX_DORMANT_SOCKETS).toBe(4);
  });

  it("parks nothing when the hidden set fits the budget", () => {
    const { dormant, parked } = dormantBudget(hidden(["a", "b"]));
    expect(dormant).toEqual(["b", "a"]);
    expect(parked).toEqual([]);
  });

  it("evicts the least recently active pane when a newer one hides", () => {
    const before = dormantBudget(hidden(["a", "b", "c", "d"]));
    expect(before.parked).toEqual([]);
    const after = dormantBudget(hidden(["a", "b", "c", "d", "e"]));
    expect(after.dormant).not.toContain("a");
    expect(after.parked).toEqual(["a"]);
  });
});

describe("pane mode", () => {
  const panes = hidden(["a", "b", "c"]);

  it("renders a visible pane even when it is not the focused one", () => {
    expect(paneMode({ hidden: false, id: "a", hiddenPanes: panes })).toBe("active");
  });

  it("buffers a hidden pane without rendering while under budget", () => {
    expect(paneMode({ hidden: true, id: "c", hiddenPanes: panes })).toBe("dormant");
  });

  it("parks a hidden pane the budget cannot hold", () => {
    const many = hidden(["a", "b", "c", "d", "e"]);
    expect(paneMode({ hidden: true, id: "a", hiddenPanes: many })).toBe("parked");
    expect(paneMode({ hidden: true, id: "e", hiddenPanes: many })).toBe("dormant");
  });
});

describe("dormant resume", () => {
  it("replays a small buffer byte-exact, without a reset", () => {
    const buffered = new Uint8Array([1, 2, 3, 4]);
    const plan = planDormantResume(buffered, buffered, buffered.byteLength);
    expect(plan.kind).toBe("delta");
    expect(plan.data).toEqual(buffered);
  });

  it("resets to a ground-state tail once the buffer passes the budget", () => {
    const budget = 1024;
    const ring = new Uint8Array(budget * 4);
    ring[10] = 0x0a;
    ring[ring.length - 1] = 0x0a;
    const plan = planDormantResume(ring, ring, ring.byteLength + budget, budget);
    expect(plan.kind).toBe("reset");
    // prepareReplayTail keeps the absolute end position and trims to a
    // ground-state seam, so the tail is strictly smaller than the ring and
    // never larger than the budget.
    expect(plan.data.byteLength).toBeLessThan(ring.byteLength);
    expect(plan.data.byteLength).toBeLessThanOrEqual(budget + 11);
  });

  it("treats a buffer exactly at the budget as a delta", () => {
    const buffered = new Uint8Array(REPLAY_TAIL_MAX_BYTES);
    const plan = planDormantResume(buffered, buffered, buffered.byteLength);
    expect(plan.kind).toBe("delta");
    expect(plan.data.byteLength).toBe(REPLAY_TAIL_MAX_BYTES);
  });
});
