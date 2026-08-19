import { afterEach, describe, expect, it, vi } from "vitest";
import { createPlaceMetrics } from "../placeMetrics";
import { type Reply, fakeVogt, unavailable } from "./harness";

afterEach(() => {
  // Keep fetch ownership scoped to each test through Vitest's global restore.
});

describe("shared place metrics", () => {
  it("keeps real zero distinct from an unavailable provider", async () => {
    fakeVogt({
      "GET /inbox": {
        body: {
          entries: [],
          snapshot_at: "2026-08-18T00:00:00Z",
          coverage: {},
          counts: { active: 0 },
        },
      },
      "GET /projects": unavailable("projects are offline"),
      "GET /work": { body: { items: [], total: 12 } },
      "GET /backlog": {
        body: { items: [], total_considered: 7, freshness: { status: "fresh", collectors: {} } },
      },
    });

    const state = createPlaceMetrics();
    await state.refresh();

    expect(state.metrics.inbox).toEqual({ value: 0, state: "ready" });
    expect(state.metrics.projects).toEqual({ value: null, state: "unavailable" });
    expect(state.metrics.board).toEqual({ value: 12, state: "ready" });
    expect(state.metrics.backlog).toEqual({ value: 7, state: "ready" });
  });

  it("keeps the last value labelled stale while a live refresh is pending", async () => {
    const vogt = fakeVogt({
      "GET /inbox": {
        body: { entries: [], snapshot_at: "2026-08-18T00:00:00Z", coverage: {}, counts: { active: 3 } },
      },
    });
    const state = createPlaceMetrics();
    await state.refresh();
    let release = () => {};
    vogt.route("GET /inbox", () => new Promise((resolve) => {
      release = () => resolve({
        body: { entries: [], snapshot_at: "2026-08-18T00:01:00Z", coverage: {}, counts: { active: 4 } },
      });
    }));

    const pending = state.refresh();
    expect(state.metrics.inbox).toEqual({ value: 3, state: "stale" });
    release();
    await pending;
    expect(state.metrics.inbox).toEqual({ value: 4, state: "ready" });
  });

  // -- what the badges cost the core (#138) --------------------------------
  //
  // Four counts per refresh is the design; four counts per *event per tab*
  // was the incident. These two tests are the floor under that: a burst is
  // one read, and a change arriving mid-read does not start a second one.

  it("folds a burst of changes into one read", async () => {
    vi.useFakeTimers();
    try {
      const vogt = fakeVogt({
        "GET /inbox": {
          body: { entries: [], snapshot_at: "2026-08-19T00:00:00Z", coverage: {}, counts: { active: 1 } },
        },
        "GET /projects": { body: { items: [], total: 2 } },
        "GET /work": { body: { items: [], total: 3 } },
        "GET /backlog": {
          body: { items: [], total_considered: 4, freshness: { status: "fresh", collectors: {} } },
        },
      });
      const state = createPlaceMetrics();
      for (let i = 0; i < 20; i += 1) state.nudge();

      // Nothing yet: the window has not closed, so twenty events have cost
      // the core nothing at all.
      expect(vogt.calls).toHaveLength(0);

      await vi.advanceTimersByTimeAsync(1000);
      expect(vogt.matching("GET /projects")).toHaveLength(1);
      expect(vogt.matching("GET /backlog")).toHaveLength(1);
      expect(state.metrics.projects).toEqual({ value: 2, state: "ready" });
      state.dispose();
    } finally {
      vi.useRealTimers();
    }
  });

  it("never has two reads in the air, and folds a change during one into a single follow-up", async () => {
    const vogt = fakeVogt({
      "GET /inbox": {
        body: { entries: [], snapshot_at: "2026-08-19T00:00:00Z", coverage: {}, counts: { active: 1 } },
      },
      "GET /work": { body: { items: [], total: 3 } },
      "GET /backlog": {
        body: { items: [], total_considered: 4, freshness: { status: "fresh", collectors: {} } },
      },
    });
    let release = () => {};
    let asked = 0;
    vogt.route("GET /projects", () => {
      asked += 1;
      if (asked > 1) return { body: { items: [], total: 9 } };
      return new Promise<Reply>((resolve) => {
        release = () => resolve({ body: { items: [], total: 2 } });
      });
    });

    const state = createPlaceMetrics();
    const first = state.refresh();
    void state.refresh();
    void state.refresh();
    expect(vogt.matching("GET /projects")).toHaveLength(1);

    release();
    await first;
    // One follow-up for the three changes that arrived while the first read
    // was open — not three, and not one still running behind the answer.
    expect(vogt.matching("GET /projects")).toHaveLength(2);
    expect(state.metrics.projects).toEqual({ value: 9, state: "ready" });
    state.dispose();
  });
});
