import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { RuntimeTransport } from "../runtimeTransport";

// WI-1413: prod showed dozens of full session lists inside a second. These
// pin the store's side of the fix: a burst of triggers is one read in flight
// plus at most one follow-up, `lagged` is debounced, and hibernate/wake patch
// the row instead of re-reading the list.

type Controller = ReadableStreamDefaultController<Uint8Array>;
const encoder = new TextEncoder();

const ROW = {
  id: "s1",
  name: "one",
  activity: "idle",
  conversation: { id: "c1" },
};

function heldTransport() {
  const streams: Controller[] = [];
  const pending: (() => void)[] = [];
  let listCalls = 0;
  let inFlight = 0;
  let maxInFlight = 0;
  const transport: RuntimeTransport = {
    request: async (input) => {
      const url = String(input instanceof Request ? input.url : input);
      if (url.includes("/api/events")) {
        const body = new ReadableStream<Uint8Array>({ start(c) { streams.push(c); } });
        return new Response(body, { status: 200, headers: { "content-type": "text/event-stream" } });
      }
      if (url.includes("/api/sessions")) {
        listCalls++;
        inFlight++;
        maxInFlight = Math.max(maxInFlight, inFlight);
        await new Promise<void>((resolve) => pending.push(resolve));
        inFlight--;
        return new Response(JSON.stringify([ROW]), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      }
      return new Response("{}", { status: 200, headers: { "content-type": "application/json" } });
    },
    openSocket: () => { throw new Error("not used"); },
  };
  return {
    transport,
    listCalls: () => listCalls,
    maxInFlight: () => maxInFlight,
    releaseAll: () => { while (pending.length) pending.shift()!(); },
    feed: (ev: object) => streams.at(-1)!.enqueue(encoder.encode(`data: ${JSON.stringify(ev)}\n\n`)),
  };
}

async function settle(ms = 0) {
  for (let i = 0; i < 6; i++) await vi.advanceTimersByTimeAsync(ms);
}

let script: ReturnType<typeof heldTransport>;
let store: typeof import("../store");

beforeEach(async () => {
  vi.useFakeTimers();
  localStorage.setItem("vogt.token", "browser-test-token");
  script = heldTransport();
  vi.resetModules();
  const runtime = await import("../runtimeTransport");
  runtime.installRuntimeTransport(script.transport);
  store = await import("../store");
});

afterEach(() => {
  store.stopEventStream();
  vi.useRealTimers();
});

async function loaded() {
  const first = store.refreshSessions();
  await settle();
  script.releaseAll();
  await first;
  await settle();
  expect(store.sessionsStore.sessions.s1).toBeDefined();
}

describe("session list coalescing", () => {
  it("turns a burst of triggers into one read in flight and one follow-up", async () => {
    const reads = Array.from({ length: 20 }, () => store.refreshSessions());
    await settle();
    expect(script.listCalls()).toBe(1);
    script.releaseAll();
    await settle();
    expect(script.listCalls()).toBe(2);
    script.releaseAll();
    await Promise.all(reads);
    await settle();
    expect(script.listCalls()).toBe(2);
    expect(script.maxInFlight()).toBe(1);
  });

  it("debounces a burst of lagged events into one read", async () => {
    await loaded();
    store.startEventStream();
    await settle();
    const before = script.listCalls();
    for (let i = 0; i < 10; i++) script.feed({ type: "lagged", skipped: 5 });
    await settle();
    expect(script.listCalls()).toBe(before);
    await settle(store.LAGGED_REFRESH_DEBOUNCE_MS);
    script.releaseAll();
    await settle();
    expect(script.listCalls()).toBe(before + 1);
  });

  it("patches the row on hibernate and wake without reading the list", async () => {
    await loaded();
    store.startEventStream();
    await settle();
    const before = script.listCalls();

    script.feed({ type: "session-hibernated", id: "s1", trigger: "idle" });
    await settle();
    expect(store.sessionsStore.sessions.s1?.activity).toBe("hibernated");
    expect(store.sessionsStore.sessions.s1?.hibernation?.trigger).toBe("idle");
    expect(store.sessionsStore.sessions.s1?.hibernation?.resumable).toBe(true);

    script.feed({ type: "session-woken", id: "s1" });
    await settle();
    expect(store.sessionsStore.sessions.s1?.activity).toBe("idle");
    expect(store.sessionsStore.sessions.s1?.hibernation).toBeNull();
    expect(script.listCalls()).toBe(before);
  });
});
