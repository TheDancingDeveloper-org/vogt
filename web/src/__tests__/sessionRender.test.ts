// WI-2696 / WI-2328 harness: a refetch must not rebuild untouched session
// rows, and an activity tick must not re-sort the rail.

import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import type { RuntimeTransport } from "../runtimeTransport";
import type { SessionSummary } from "../api";

function row(id: string, over: Partial<SessionSummary> = {}): SessionSummary {
  return {
    id,
    name: id,
    activity: "idle",
    exit_code: null,
    scrollback_bytes: 0,
    cwd: "/workspace",
    created_at: "2026-08-18T08:00:00Z",
    role: "worker",
    resources: { rss_bytes: 1, cpu_pct: 0, processes: 1, sampled_at: "2026-08-18T08:00:00Z" },
    ...over,
  };
}

function transport(rows: () => SessionSummary[]): { transport: RuntimeTransport; release: () => void } {
  const pending: (() => void)[] = [];
  const transport: RuntimeTransport = {
    request: async (input) => {
      const url = String(input instanceof Request ? input.url : input);
      if (url.includes("/api/sessions")) {
        await new Promise<void>((resolve) => pending.push(resolve));
        return new Response(JSON.stringify({ sessions: rows(), hidden_count: 0 }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      }
      if (url.includes("/api/events")) {
        const body = new ReadableStream<Uint8Array>({ start(c) { void c; } });
        return new Response(body, { status: 200, headers: { "content-type": "text/event-stream" } });
      }
      return new Response("{}", { status: 200, headers: { "content-type": "application/json" } });
    },
    openSocket: () => { throw new Error("not used"); },
  };
  return { transport, release: () => { while (pending.length) pending.shift()!(); } };
}

async function settle() {
  for (let i = 0; i < 4; i++) await vi.advanceTimersByTimeAsync(0);
}

describe("session rail refetch identity (WI-2696)", () => {
  let store: typeof import("../store");
  let rail: typeof import("../railSessions");
  let script: ReturnType<typeof transport>;
  let rows: SessionSummary[];

  beforeEach(async () => {
    vi.useFakeTimers();
    localStorage.setItem("vogt.token", "browser-test-token");
    rows = [row("keep"), row("also")];
    script = transport(() => rows);
    vi.resetModules();
    const runtime = await import("../runtimeTransport");
    runtime.installRuntimeTransport(script.transport);
    store = await import("../store");
    rail = await import("../railSessions");
  });

  afterEach(() => {
    store.stopEventStream();
    vi.useRealTimers();
  });

  async function load() {
    const pending = store.refreshSessions();
    await settle();
    script.release();
    await pending;
  }

  it("keeps untouched row identity across a refetch", async () => {
    await load();
    const before = store.sessionsStore.sessions.keep;
    const also = store.sessionsStore.sessions.also;
    expect(before).toBeDefined();
    rows = [row("keep", { name: "kept" }), row("also", { name: "renamed" })];
    const started = performance.now();
    await load();
    const elapsed = performance.now() - started;
    // Reconcile writes the diff into the object already held, so a changed
    // name does not replace the row (and does not remount its DOM).
    expect(store.sessionsStore.sessions.keep).toBe(before);
    expect(store.sessionsStore.sessions.also).toBe(also);
    expect(store.sessionsStore.sessions.also?.name).toBe("renamed");
    expect(store.sessionsStore.sessions.keep?.name).toBe("kept");
    expect(elapsed).toBeLessThanOrEqual(5);
  });

  it("does not rebuild the rail array when only resources change", async () => {
    await load();
    const first = rail.railSessions();
    rows = [
      row("keep", { name: "kept", resources: { rss_bytes: 9, cpu_pct: 4, processes: 2, sampled_at: "2026-08-18T08:00:10Z" } }),
      row("also", { name: "also-2" }),
    ];
    await load();
    expect(rail.railSessions()).toBe(first);
    expect(rail.railSessions()[0]?.resources?.rss_bytes).toBe(9);
  });

  it("drops a row that left the list and keeps the one that stayed", async () => {
    await load();
    const before = store.sessionsStore.sessions.keep;
    rows = [row("keep"), row("newcomer")];
    await load();
    expect(store.sessionsStore.sessions.keep).toBe(before);
    expect(store.sessionsStore.sessions.also).toBeUndefined();
    expect(store.sessionsStore.order).toEqual(["keep", "newcomer"]);
  });
});
