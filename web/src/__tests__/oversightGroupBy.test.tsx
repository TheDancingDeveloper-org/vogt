// WI-4747: oversight board group-by toggle, attention (default) / work item.

import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import Oversight, { groupByWorkItem } from "../Oversight";
import * as vogtApi from "../vogtApi";

const ROW = (
  id: string,
  attention: vogtApi.SessionSweepRow["attention"],
  wi?: string,
  title?: string,
): vogtApi.SessionSweepRow => ({
  attention,
  attention_reason: `reason ${id}`,
  session: {
    id,
    engine_session_id: `uuid-${id}`,
    actor: `agent:session:${id}`,
    cwd: "/w",
    reason: "x",
    started_at: "2026-10-05T00:00:00Z",
    work_item: wi ?? null,
    work_item_title: title ?? null,
  },
  screen_tail: [],
});

const ROWS: vogtApi.SessionSweepRow[] = [
  ROW("s1", "approval", "WI-9", "Nine title"),
  ROW("s2", "running"),
  ROW("s3", "blocked", "WI-5"),
  ROW("s4", "idle", "WI-9", "Nine title"),
];

function mockSweep() {
  vi.spyOn(vogtApi, "sweepSessions").mockResolvedValue({
    rows: ROWS,
    counts: { total: 4, needs_you: 2 },
    swept_at: new Date().toISOString(),
    engine: null,
  });
}

beforeEach(() => localStorage.clear());
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("groupByWorkItem", () => {
  it("orders groups by worst attention and collects rows without a work item", () => {
    const groups = groupByWorkItem([ROWS[1]!, ROWS[2]!, ROWS[0]!, ROWS[3]!]);
    expect(groups.map((g) => g.ref)).toEqual(["WI-9", "WI-5", null]);
    expect(groups[0]!.rows.map((r) => r.session.id)).toEqual(["s1", "s4"]);
    expect(groups[0]!.title).toBe("Nine title");
  });
});

describe("Oversight group-by toggle", () => {
  it("defaults to attention order with no group headers", async () => {
    mockSweep();
    render(() => <Oversight />);
    await screen.findByText("reason s1");
    expect(screen.getAllByText(/^reason /).map((e) => e.textContent)).toEqual([
      "reason s1",
      "reason s2",
      "reason s3",
      "reason s4",
    ]);
    expect(screen.queryByText("No work item")).toBeNull();
    expect(screen.getByRole("button", { name: "Attention" }).getAttribute("aria-pressed")).toBe("true");
  });

  it("groups by work item with ref and title headers and a No work item group", async () => {
    mockSweep();
    render(() => <Oversight />);
    await screen.findByText("reason s1");
    fireEvent.click(screen.getByRole("button", { name: "Work item" }));
    const headers = screen.getAllByRole("heading", { level: 3 }).map((h) => h.textContent);
    expect(headers[0]).toContain("WI-9");
    expect(headers[0]).toContain("Nine title");
    expect(headers[1]).toContain("WI-5");
    expect(headers[2]).toContain("No work item");
    expect(screen.getAllByText(/^reason /).map((e) => e.textContent)).toEqual([
      "reason s1",
      "reason s4",
      "reason s3",
      "reason s2",
    ]);
  });

  it("remembers the choice for the viewer", async () => {
    mockSweep();
    const first = render(() => <Oversight />);
    await screen.findByText("reason s1");
    fireEvent.click(screen.getByRole("button", { name: "Work item" }));
    first.unmount();
    render(() => <Oversight />);
    await screen.findByText("reason s1");
    expect(screen.getByRole("button", { name: "Work item" }).getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByText("No work item")).toBeTruthy();
  });

  it("still works when storage throws", async () => {
    mockSweep();
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("blocked");
    });
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("blocked");
    });
    render(() => <Oversight />);
    await screen.findByText("reason s1");
    fireEvent.click(screen.getByRole("button", { name: "Work item" }));
    expect(screen.getByText("No work item")).toBeTruthy();
  });
});
