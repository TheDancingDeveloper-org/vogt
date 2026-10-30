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

describe("vogt-changed coalescing (WI-1254)", () => {
  const change = (seq: number, id = "WI-1", kind = "work.updated") => ({
    type: "vogt-changed",
    kind,
    entity_kind: "work_item",
    entity_id: id,
    seq,
  });

  it("nudges each seq listener once per burst, with the latest seq", async () => {
    store.startEventStream();
    await settle();
    const a = vi.fn();
    const b = vi.fn();
    const stopA = store.onVogtChanged(a);
    const stopB = store.onVogtChanged(b);
    for (let seq = 1; seq <= 8; seq++) script.feed(change(seq, `WI-${seq % 3}`));
    await settle();
    expect(a).not.toHaveBeenCalled();
    await settle(store.VOGT_CHANGED_COALESCE_MS);
    expect(a.mock.calls).toEqual([[8]]);
    expect(b.mock.calls).toEqual([[8]]);
    stopA();
    stopB();
  });

  it("hands event listeners each distinct change once, latest wins", async () => {
    store.startEventStream();
    await settle();
    const seen: Array<[string, string, number]> = [];
    const stop = store.onVogtChangedEvent((e) => seen.push([e.entity_id, e.kind, e.seq]));
    script.feed(change(1, "WI-1"));
    script.feed(change(2, "WI-2"));
    script.feed(change(3, "WI-1"));
    script.feed(change(4, "WI-1", "work.transitioned"));
    await settle(store.VOGT_CHANGED_COALESCE_MS);
    expect(seen).toEqual([
      ["WI-2", "work.updated", 2],
      ["WI-1", "work.updated", 3],
      ["WI-1", "work.transitioned", 4],
    ]);
    stop();
  });

  it("flushes a steady drip by the max wait, not never", async () => {
    store.startEventStream();
    await settle();
    const a = vi.fn();
    const stop = store.onVogtChanged(a);
    const step = store.VOGT_CHANGED_COALESCE_MS - 50;
    let seq = 0;
    for (let t = 0; t < store.VOGT_CHANGED_MAX_WAIT_MS; t += step) {
      script.feed(change(++seq));
      await vi.advanceTimersByTimeAsync(step);
    }
    await settle();
    expect(a.mock.calls.length).toBe(1);
    expect(a.mock.calls[0]?.[0]).toBeLessThanOrEqual(seq);
    stop();
  });

  it("drops a pending burst when the stream stops", async () => {
    store.startEventStream();
    await settle();
    const a = vi.fn();
    const stop = store.onVogtChanged(a);
    script.feed(change(1));
    await settle();
    store.stopEventStream();
    await settle(store.VOGT_CHANGED_COALESCE_MS);
    expect(a).not.toHaveBeenCalled();
    stop();
  });
});
