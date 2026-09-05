import { afterEach, describe, expect, it, vi } from "vitest";
import { RETRY_BASE_MS, createPlaceMetrics } from "../placeMetrics";
import { DEADLINE_MS } from "../deadlines";
import { type Reply, fakeVogt, held, unavailable } from "./harness";

afterEach(() => {
  // Keep fetch ownership scoped to each test through Vitest's global restore.
});

describe("shared place metrics", () => {
  it("uses one aggregate request for every badge", async () => {
    const vogt = fakeVogt({
      "GET /place/metrics": {
        body: {
          inbox_active: 1,
          projects_total: 2,
          work_total: 3,
          backlog_total_considered: 4,
          drift_present: true,
          revision: 9,
          generated_at: "2026-08-19T00:00:00Z",
        },
      },
    });

    const state = createPlaceMetrics();
    await state.refresh();

    expect(vogt.matching("GET /place/metrics")).toHaveLength(1);
    expect(vogt.calls).toHaveLength(1);
    expect(state.metrics).toMatchObject({
      inbox: { value: 1, state: "ready" },
      projects: { value: 2, state: "ready" },
      board: { value: 3, state: "ready" },
      backlog: { value: 4, state: "ready" },
      drift: { value: 1, state: "ready" },
    });
    state.dispose();
  });

  it("marks only null aggregate fields unavailable", async () => {
    fakeVogt({
      "GET /place/metrics": {
        body: {
          inbox_active: null,
          projects_total: 2,
          work_total: null,
          backlog_total_considered: 4,
          drift_present: false,
          revision: 9,
          generated_at: "2026-08-19T00:00:00Z",
        },
      },
    });

    const state = createPlaceMetrics();
    await state.refresh();

    expect(state.metrics.inbox).toEqual({ value: null, state: "unavailable" });
    expect(state.metrics.board).toEqual({ value: null, state: "unavailable" });
    expect(state.metrics.projects).toEqual({ value: 2, state: "ready" });
    expect(state.metrics.backlog).toEqual({ value: 4, state: "ready" });
    expect(state.metrics.drift).toEqual({ value: 0, state: "ready" });
    state.dispose();
  });

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
        "GET /place/metrics": {
          body: {
            inbox_active: 1,
            projects_total: 2,
            work_total: 3,
            backlog_total_considered: 4,
            drift_present: false,
            revision: 1,
            generated_at: "2026-08-19T00:00:00Z",
          },
        },
      });
      const state = createPlaceMetrics();
      for (let i = 0; i < 20; i += 1) state.nudge();

      // Nothing yet: the window has not closed, so twenty events have cost
      // the core nothing at all.
      expect(vogt.calls).toHaveLength(0);

      await vi.advanceTimersByTimeAsync(1000);
      expect(vogt.matching("GET /place/metrics")).toHaveLength(1);
      expect(state.metrics.projects).toEqual({ value: 2, state: "ready" });
      state.dispose();
    } finally {
      vi.useRealTimers();
    }
  });

  // -- a read that does not answer (#581) ----------------------------------
  //
  // On prod the aggregate took 9 s against an 8 s deadline. Every tab aborted
  // at the deadline and re-asked on the next event, every ~8 s, while the
  // core still ran each abandoned read to completion. These pin the two
  // rules that stop that: a failed read backs off, doubling; and what was
  // known stays on screen as stale rather than vanishing.

  it("backs off after a read exceeds its deadline, and retries on its own", async () => {
    vi.useFakeTimers();
    try {
      const vogt = fakeVogt({ "GET /place/metrics": held().handler });
      const state = createPlaceMetrics();
      state.nudge();
      await vi.advanceTimersByTimeAsync(1000);
      expect(vogt.matching("GET /place/metrics")).toHaveLength(1);

      // The deadline passes: the read fails, nothing is known, so the badges
      // are unavailable — and a burst of events changes nothing.
      await vi.advanceTimersByTimeAsync(DEADLINE_MS.list + 100);
      expect(state.metrics.inbox).toEqual({ value: null, state: "unavailable" });
      for (let i = 0; i < 20; i += 1) state.nudge();
      await vi.advanceTimersByTimeAsync(1000);
      expect(vogt.matching("GET /place/metrics")).toHaveLength(1);

      // The wait ends and the badges ask again by themselves, once.
      vogt.route("GET /place/metrics", { body: { projects_total: 5, revision: 2 } });
      await vi.advanceTimersByTimeAsync(RETRY_BASE_MS);
      expect(vogt.matching("GET /place/metrics")).toHaveLength(2);
      expect(state.metrics.projects).toEqual({ value: 5, state: "ready" });

      // Recovered: the next event reads promptly again.
      state.nudge();
      await vi.advanceTimersByTimeAsync(1000);
      expect(vogt.matching("GET /place/metrics")).toHaveLength(3);
      state.dispose();
    } finally {
      vi.useRealTimers();
    }
  });

  it("doubles the wait while reads keep failing, and keeps the last values as stale", async () => {
    vi.useFakeTimers();
    try {
      const vogt = fakeVogt({ "GET /place/metrics": { body: { projects_total: 2, revision: 1 } } });
      const state = createPlaceMetrics();
      await state.refresh();
      expect(state.metrics.projects).toEqual({ value: 2, state: "ready" });

      vogt.route("GET /place/metrics", unavailable("core is not answering"));
      state.nudge();
      await vi.advanceTimersByTimeAsync(1000);
      expect(vogt.matching("GET /place/metrics")).toHaveLength(2);
      // A count from a moment ago beats a dash.
      expect(state.metrics.projects).toEqual({ value: 2, state: "stale" });

      // First wait: 8 s. Second: 16 s. Events in between are dropped.
      await vi.advanceTimersByTimeAsync(RETRY_BASE_MS - 500);
      state.nudge();
      await vi.advanceTimersByTimeAsync(1000);
      expect(vogt.matching("GET /place/metrics")).toHaveLength(3);
      await vi.advanceTimersByTimeAsync(RETRY_BASE_MS);
      state.nudge();
      await vi.advanceTimersByTimeAsync(1000);
      expect(vogt.matching("GET /place/metrics")).toHaveLength(3);
      await vi.advanceTimersByTimeAsync(RETRY_BASE_MS);
      expect(vogt.matching("GET /place/metrics")).toHaveLength(4);
      expect(state.metrics.projects).toEqual({ value: 2, state: "stale" });
      state.dispose();
    } finally {
      vi.useRealTimers();
    }
  });

  it("never has two reads in the air, and folds a change during one into a single follow-up", async () => {
    const vogt = fakeVogt({
      "GET /place/metrics": unavailable("place metrics are unavailable"),
    });
    let release = () => {};
    let asked = 0;
    vogt.route("GET /place/metrics", () => {
      asked += 1;
      if (asked > 1) return { body: { projects_total: 9, revision: 2 } };
      return new Promise<Reply>((resolve) => {
        release = () => resolve({ body: { projects_total: 2, revision: 1 } });
      });
    });

    const state = createPlaceMetrics();
    const first = state.refresh();
    void state.refresh();
    void state.refresh();
    expect(vogt.matching("GET /place/metrics")).toHaveLength(1);

    release();
    await first;
    // One follow-up for the three changes that arrived while the first read
    // was open — not three, and not one still running behind the answer.
    expect(vogt.matching("GET /place/metrics")).toHaveLength(2);
    expect(state.metrics.projects).toEqual({ value: 9, state: "ready" });
    state.dispose();
  });
});
