import { afterEach, describe, expect, it } from "vitest";
import { createPlaceMetrics } from "../placeMetrics";
import { fakeVogt, unavailable } from "./harness";

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
});
