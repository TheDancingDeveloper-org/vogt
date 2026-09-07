import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { SessionSummary } from "../api";
import {
  MAX_PREWARM_SESSIONS,
  beginForegroundReplay,
  createPrewarmCoordinator,
  foregroundReplayActive,
  resetForegroundReplayForTest,
  selectPrewarmTargets,
} from "../terminalPrewarm";

function session(
  id: string,
  activityChangedAt: string,
  extra: Partial<SessionSummary> = {},
): SessionSummary {
  return {
    id,
    name: id,
    activity: "idle",
    exit_code: null,
    scrollback_bytes: 0,
    cwd: "/",
    created_at: activityChangedAt,
    activity_changed_at: activityChangedAt,
    ...extra,
  };
}

const iso = (minutesAgo: number) =>
  new Date(Date.UTC(2026, 0, 1, 0, 0) - minutesAgo * 60_000).toISOString();

describe("selectPrewarmTargets", () => {
  it("orders most-recently-active first and caps at the limit", () => {
    const sessions = [
      session("old", iso(30)),
      session("newest", iso(1)),
      session("mid", iso(10)),
    ];
    const targets = selectPrewarmTargets(sessions, new Set(), new Set());
    expect(targets).toEqual(["newest", "mid", "old"]);
  });

  it("caps at MAX_PREWARM_SESSIONS", () => {
    const sessions = Array.from({ length: 20 }, (_, i) =>
      session(`s${i}`, iso(i)),
    );
    const targets = selectPrewarmTargets(sessions, new Set(), new Set());
    expect(targets).toHaveLength(MAX_PREWARM_SESSIONS);
    expect(targets[0]).toBe("s0"); // most recent
  });

  it("excludes already-opened, already-warmed and exited sessions", () => {
    const sessions = [
      session("open", iso(1)),
      session("warmed", iso(2)),
      session("exited", iso(3), { exit_code: 0 }),
      session("target", iso(4)),
    ];
    const targets = selectPrewarmTargets(
      sessions,
      new Set(["open"]),
      new Set(["warmed"]),
    );
    expect(targets).toEqual(["target"]);
  });
});

describe("foreground replay gate", () => {
  beforeEach(() => resetForegroundReplayForTest());

  it("composes overlapping tokens and is idempotent", () => {
    expect(foregroundReplayActive()).toBe(false);
    const a = beginForegroundReplay();
    const b = beginForegroundReplay();
    expect(foregroundReplayActive()).toBe(true);
    a();
    a(); // idempotent
    expect(foregroundReplayActive()).toBe(true);
    b();
    expect(foregroundReplayActive()).toBe(false);
  });
});

describe("prewarm coordinator", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resetForegroundReplayForTest();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  const twoSessions = () => [session("b", iso(2)), session("a", iso(1))];

  it("warms up to the limit, most-recent first, sequentially", async () => {
    const order: string[] = [];
    const coordinator = createPrewarmCoordinator({
      listSessions: () => twoSessions(),
      openSessionIds: () => new Set(),
      isVisible: () => true,
      foregroundActive: () => false,
      warmAttach: async (id) => {
        order.push(id);
        return true;
      },
      startDelayMs: 0,
    });
    coordinator.start();
    await vi.runAllTimersAsync();
    await coordinator.idle();
    expect(order).toEqual(["a", "b"]);
    expect([...coordinator.warmed()].sort()).toEqual(["a", "b"]);
  });

  it("does not run while the document is hidden", async () => {
    const order: string[] = [];
    let visible = false;
    const coordinator = createPrewarmCoordinator({
      listSessions: () => twoSessions(),
      openSessionIds: () => new Set(),
      isVisible: () => visible,
      foregroundActive: () => false,
      warmAttach: async (id) => {
        order.push(id);
        return true;
      },
      startDelayMs: 0,
    });
    coordinator.start();
    await vi.runAllTimersAsync();
    await coordinator.idle();
    expect(order).toEqual([]);

    // Becoming visible and kicking resumes it.
    visible = true;
    coordinator.kick();
    await vi.runAllTimersAsync();
    await coordinator.idle();
    expect(order.sort()).toEqual(["a", "b"]);
  });

  it("pauses while a foreground replay is active and resumes after", async () => {
    const order: string[] = [];
    let foreground = true;
    const coordinator = createPrewarmCoordinator({
      listSessions: () => twoSessions(),
      openSessionIds: () => new Set(),
      isVisible: () => true,
      foregroundActive: () => foreground,
      warmAttach: async (id) => {
        order.push(id);
        return true;
      },
      startDelayMs: 0,
    });
    coordinator.start();
    await vi.runAllTimersAsync();
    await coordinator.idle();
    // Foreground pane is mid-replay: nothing warmed yet.
    expect(order).toEqual([]);

    foreground = false;
    coordinator.kick();
    await vi.runAllTimersAsync();
    await coordinator.idle();
    expect(order.sort()).toEqual(["a", "b"]);
  });

  it("re-checks the gate between each attach", async () => {
    const order: string[] = [];
    let foreground = false;
    const coordinator = createPrewarmCoordinator({
      listSessions: () => [
        session("c", iso(3)),
        session("b", iso(2)),
        session("a", iso(1)),
      ],
      openSessionIds: () => new Set(),
      isVisible: () => true,
      foregroundActive: () => foreground,
      warmAttach: async (id) => {
        order.push(id);
        // A foreground open lands while the first warm attach is in flight.
        if (id === "a") foreground = true;
        return true;
      },
      startDelayMs: 0,
    });
    coordinator.start();
    await vi.runAllTimersAsync();
    await coordinator.idle();
    // Only the first attach ran; the loop paused before the second.
    expect(order).toEqual(["a"]);

    foreground = false;
    coordinator.kick();
    await vi.runAllTimersAsync();
    await coordinator.idle();
    expect(order).toEqual(["a", "b", "c"]);
  });

  it("stop() aborts and prevents further passes", async () => {
    const order: string[] = [];
    const coordinator = createPrewarmCoordinator({
      listSessions: () => twoSessions(),
      openSessionIds: () => new Set(),
      isVisible: () => true,
      foregroundActive: () => false,
      warmAttach: async (id) => {
        order.push(id);
        return true;
      },
      startDelayMs: 0,
    });
    coordinator.start();
    coordinator.stop();
    await vi.runAllTimersAsync();
    await coordinator.idle();
    expect(order).toEqual([]);
  });
});
