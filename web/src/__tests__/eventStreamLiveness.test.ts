import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { RuntimeTransport } from "../runtimeTransport";

// A warm open of the phone app showed "Disconnected" for minutes while a
// session was plainly running (2026-09-05). The stream had reconnected fine;
// the flag simply had no way back to true except an event or a session-list
// refresh, and the refresh had met a network still waking from resume. These
// pin the three liveness signals the store now honours: the stream opening,
// the engine's `:ka` keep-alive comments, and silence past the deadline.

type Controller = ReadableStreamDefaultController<Uint8Array>;
const encoder = new TextEncoder();

/** A scripted transport: `/api/events` is a stream this test feeds; the
 *  session list answers whatever `sessionsStatus` says at the time. */
function scriptedTransport() {
  const streams: Controller[] = [];
  let sessionsStatus = 200;
  let sessionsHang = false;
  const requests: string[] = [];
  const transport: RuntimeTransport = {
    request: async (input, init) => {
      const url = String(input instanceof Request ? input.url : input);
      requests.push(url);
      if (url.includes("/api/sessions") && sessionsHang) {
        // A transport that never settles and only honours abort — the shape
        // of a tunnel that swallowed the request.
        return new Promise<Response>((_, reject) => {
          init?.signal?.addEventListener("abort", () =>
            reject(new DOMException("aborted", "AbortError")));
        });
      }
      if (url.includes("/api/events")) {
        const body = new ReadableStream<Uint8Array>({
          start(controller) { streams.push(controller); },
        });
        return new Response(body, { status: 200, headers: { "content-type": "text/event-stream" } });
      }
      if (url.includes("/api/sessions")) {
        return sessionsStatus === 200
          ? new Response(JSON.stringify([]), { status: 200, headers: { "content-type": "application/json" } })
          : new Response("boom", { status: sessionsStatus });
      }
      return new Response("{}", { status: 200, headers: { "content-type": "application/json" } });
    },
    openSocket: () => { throw new Error("not used"); },
  };
  return {
    transport,
    requests,
    setSessionsStatus: (status: number) => { sessionsStatus = status; },
    setSessionsHang: (hang: boolean) => { sessionsHang = hang; },
    sessionRequests: () => requests.filter((url) => url.includes("/api/sessions")).length,
    feed: (text: string) => streams.at(-1)!.enqueue(encoder.encode(text)),
    streamCount: () => streams.length,
  };
}

async function settle(ms = 0) {
  // Let the fetch-based reader loop turn over: each `await` in it is a
  // microtask, and the response resolution needs a macrotask.
  for (let i = 0; i < 6; i++) await vi.advanceTimersByTimeAsync(ms);
}

let script: ReturnType<typeof scriptedTransport>;
let store: typeof import("../store");

beforeEach(async () => {
  vi.useFakeTimers();
  localStorage.setItem("vogt.token", "browser-test-token");
  script = scriptedTransport();
  // A fresh store per test means a fresh module graph; the transport must
  // be installed on the instance that graph will import, not on this
  // file's own.
  vi.resetModules();
  const runtime = await import("../runtimeTransport");
  runtime.installRuntimeTransport(script.transport);
  store = await import("../store");
});

afterEach(() => {
  store.stopEventStream();
  vi.useRealTimers();
});

describe("event stream liveness", () => {
  it("counts the stream opening as connected, before any event arrives", async () => {
    store.startEventStream();
    expect(store.isConnected()).toBe(false);
    await settle();
    expect(store.isConnected()).toBe(true);
    expect(store.sessionsStore.lastAnswerAt).not.toBeNull();
  });

  it("keeps a quiet stream connected on the engine's keep-alive comments alone", async () => {
    store.startEventStream();
    await settle();
    const before = store.sessionsStore.lastAnswerAt;
    await vi.advanceTimersByTimeAsync(15_000);
    script.feed(":ka\n\n");
    await settle();
    expect(store.isConnected()).toBe(true);
    expect(store.sessionsStore.lastAnswerAt).not.toBe(before);
    expect(script.streamCount()).toBe(1);
  });

  it("heals a session list that failed while the network was still waking", async () => {
    script.setSessionsStatus(503);
    await store.refreshSessions();
    expect(store.sessionsError()).not.toBeNull();
    expect(store.isConnected()).toBe(false);

    script.setSessionsStatus(200);
    store.startEventStream();
    await settle();
    expect(store.isConnected()).toBe(true);
    expect(store.sessionsError()).toBeNull();
    expect(store.sessionsStore.ready).toBe(true);
  });

  it("lets a live stream outrank a session list that fails afterwards", async () => {
    store.startEventStream();
    await settle();
    expect(store.isConnected()).toBe(true);
    script.setSessionsStatus(503);
    await store.refreshSessions();
    // The list is reported as failed; the front door is still plainly there.
    expect(store.sessionsError()).not.toBeNull();
    expect(store.isConnected()).toBe(true);
  });

  it("presumes a silent stream dead after three missed keep-alives and reconnects", async () => {
    store.startEventStream();
    await settle();
    expect(script.streamCount()).toBe(1);
    await vi.advanceTimersByTimeAsync(store.EVENT_STREAM_STALE_MS - 1);
    expect(script.streamCount()).toBe(1);
    await vi.advanceTimersByTimeAsync(1);
    await settle();
    expect(script.streamCount()).toBe(2);
    expect(store.isConnected()).toBe(true);
  });

  // #591: the Places rail sat on "…" with an empty Running list and no error.
  // A list aborted by a newer wake reports neither success nor failure, and
  // the stream-open retry only asked "did the last list fail?".
  it("lands a session list whose boot read was aborted by a newer wake", async () => {
    const boot = new AbortController();
    const pending = store.refreshSessions(boot.signal);
    boot.abort();
    await pending;
    expect(store.sessionsStore.ready).toBe(false);
    expect(store.sessionsError()).toBeNull();

    store.startEventStream();
    await settle();
    expect(store.sessionsStore.ready).toBe(true);
    expect(store.sessionsError()).toBeNull();
  });

  it("does not re-read a list that already landed when the stream opens", async () => {
    await store.refreshSessions();
    const before = script.sessionRequests();
    store.startEventStream();
    await settle();
    expect(script.sessionRequests()).toBe(before);
  });

  it("states a list that never landed as a fault after a while, and asks again", async () => {
    // The transport's own deadline already turns a *hung* read into an error;
    // this is the read nothing is waiting on any more — aborted by a newer
    // wake, no stream open to trigger the retry — which used to leave "…"
    // in the rail for good.
    const boot = new AbortController();
    const pending = store.refreshSessions(boot.signal);
    boot.abort();
    await pending;
    const before = script.sessionRequests();

    script.setSessionsHang(true);
    await vi.advanceTimersByTimeAsync(store.SESSION_LIST_STALL_MS - 1);
    expect(store.sessionsError()).toBeNull();
    await vi.advanceTimersByTimeAsync(1);
    // Stated, with the reason, rather than an ellipsis forever…
    expect(store.sessionsError()).toMatch(/has not answered/);
    expect(store.sessionsStore.ready).toBe(false);
    // …and a fresh list is asked for.
    expect(script.sessionRequests()).toBe(before + 1);

    script.setSessionsHang(false);
    await store.refreshSessions();
    expect(store.sessionsStore.ready).toBe(true);
    expect(store.sessionsError()).toBeNull();
  });

  it("does not presume death while frames keep coming", async () => {
    store.startEventStream();
    await settle();
    for (let i = 0; i < 4; i++) {
      await vi.advanceTimersByTimeAsync(20_000);
      script.feed(":ka\n\n");
      await settle();
    }
    expect(script.streamCount()).toBe(1);
    expect(store.isConnected()).toBe(true);
  });
});
