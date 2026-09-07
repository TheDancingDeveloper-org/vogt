// The session-row model (sessionRowModel.ts): the words a row uses and the
// order a list of them is shown in. These are the non-trivial parts the rail
// and both SessionList callers share, so they are pinned once here rather than
// re-asserted through three surfaces' DOM.

import { describe, expect, it } from "vitest";
import type { SessionSummary } from "../api";
import {
  activityClass,
  activityLabel,
  attentionRank,
  sessionStateWord,
  sortSessionsForRail,
  sortSessionsByAttention,
} from "../sessionRowModel";

function session(over: Partial<SessionSummary> & { id: string }): SessionSummary {
  return {
    id: over.id,
    name: over.name ?? over.id,
    activity: over.activity ?? "idle",
    exit_code: over.exit_code ?? null,
    scrollback_bytes: 0,
    cwd: over.cwd ?? "/workspace",
    created_at: over.created_at ?? "2026-08-18T08:00:00Z",
    activity_changed_at: over.activity_changed_at,
  };
}

describe("activity labels and classes", () => {
  it("reads a finished session as finished, exit code first", () => {
    expect(activityLabel("running", 0)).toBe("exited (0)");
    expect(activityLabel("running", 2)).toBe("errored (2)");
    expect(activityClass(session({ id: "a", activity: "running", exit_code: 0 }))).toBe("done");
    expect(activityClass(session({ id: "b", activity: "running", exit_code: 2 }))).toBe("errored");
  });

  it("spells out waiting-for-input and keeps other states verbatim", () => {
    expect(activityLabel("waiting-for-input", null)).toBe("waiting for input");
    expect(activityLabel("idle", null)).toBe("idle");
    expect(activityClass(session({ id: "c", activity: "waiting-for-input" }))).toBe("waiting-for-input");
  });
});

describe("sessionStateWord", () => {
  const now = Date.parse("2026-08-18T08:01:00Z");

  it("appends an age when the engine reported one", () => {
    const word = sessionStateWord(
      session({ id: "d", activity: "running", activity_changed_at: "2026-08-18T08:00:00Z" }),
      now,
    );
    expect(word).toMatch(/^running · /);
  });

  it("omits the age rather than guessing when it is absent", () => {
    expect(sessionStateWord(session({ id: "e", activity: "idle" }), now)).toBe("idle");
  });
});

describe("attention order", () => {
  it("ranks waiting-for-input ahead of errors, runs, idles and clean exits", () => {
    expect(attentionRank(session({ id: "w", activity: "waiting-for-input" })))
      .toBeLessThan(attentionRank(session({ id: "r", activity: "running" })));
    expect(attentionRank(session({ id: "err", activity: "running", exit_code: 1 })))
      .toBeLessThan(attentionRank(session({ id: "run", activity: "running" })));
    // A clean exit sinks to the very bottom regardless of last activity.
    expect(attentionRank(session({ id: "done", activity: "running", exit_code: 0 })))
      .toBeGreaterThan(attentionRank(session({ id: "idle", activity: "idle" })));
  });

  it("sorts attention first, then most-recently-active, without mutating input", () => {
    const idleOld = session({ id: "idle-old", activity: "idle", activity_changed_at: "2026-08-18T08:00:00Z" });
    const idleNew = session({ id: "idle-new", activity: "idle", activity_changed_at: "2026-08-18T09:00:00Z" });
    const waiting = session({ id: "waiting", activity: "waiting-for-input" });
    const input = [idleOld, idleNew, waiting];
    const sorted = sortSessionsByAttention(input);
    expect(sorted.map((s) => s.id)).toEqual(["waiting", "idle-new", "idle-old"]);
    // A copy: the caller's array is left in place.
    expect(input.map((s) => s.id)).toEqual(["idle-old", "idle-new", "waiting"]);
  });

  it("partitions bookmarks without rebuilding bookmark state per comparison", () => {
    const rows = [
      session({ id: "idle-old", activity: "idle", activity_changed_at: "2026-08-18T08:00:00Z" }),
      session({ id: "waiting", activity: "waiting-for-input" }),
      session({ id: "idle-new", activity: "idle", activity_changed_at: "2026-08-18T09:00:00Z" }),
    ];
    expect(sortSessionsForRail(rows, new Set(["idle-old"])).map((row) => row.id))
      .toEqual(["idle-old", "waiting", "idle-new"]);
  });
});
