import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type * as Store from "../store";
import type * as Wake from "../wakeCoordinator";

// WI-2324: vogt.sprooty.com reconnected /api/events every 3–5 s and re-read
// the 208 KB session list each time. A second tab on the same Firefox profile
// held the URL's cache entry, so this tab's connect never got its headers;
// every focus edge then found the stream "unhealthy", tore the pending
// connect down and sent another, and every focus edge read the whole list
// again. These pin the wake side: a live or pending stream is left alone, and
// a focus edge does not re-read a list the stream is keeping current.

const mocks = vi.hoisted(() => ({
  streams: [] as Array<{
    onError: () => void;
    hooks: { onOpen?: () => void; onHeartbeat?: () => void };
    unsubscribe: ReturnType<typeof vi.fn>;
  }>,
  listSessions: vi.fn(),
}));

vi.mock("../api", () => ({
  api: { listSessions: (...args: unknown[]) => mocks.listSessions(...args) },
  subscribeEvents: (
    _onEvent: unknown,
    onError: () => void,
    hooks: { onOpen?: () => void; onHeartbeat?: () => void } = {},
  ) => {
    const stream = { onError, hooks, unsubscribe: vi.fn() };
    mocks.streams.push(stream);
    return stream.unsubscribe;
  },
}));

const last = () => {
  const stream = mocks.streams.at(-1);
  if (!stream) throw new Error("no event stream was subscribed");
  return stream;
};
const open = () => last().hooks.onOpen?.();
const heartbeat = () => last().hooks.onHeartbeat?.();

let store: typeof Store;
let wake: typeof Wake;

/** Note one lifecycle edge and let its debounced wake fire. */
async function edge(reason: Wake.WakeReason, hiddenMs = 0) {
  wake.noteForeground(reason, hiddenMs);
  await vi.advanceTimersByTimeAsync(250);
}

beforeEach(async () => {
  vi.resetModules();
  vi.useFakeTimers();
  store = await import("../store");
  wake = await import("../wakeCoordinator");
  mocks.streams.length = 0;
  mocks.listSessions.mockReset();
  mocks.listSessions.mockResolvedValue({ sessions: [], hidden_count: 0 });
  // The list has landed once, as it has by the time anyone is using the page.
  await store.refreshSessions();
  mocks.listSessions.mockClear();
});

afterEach(() => {
  store.stopEventStream();
  wake.cancelPendingWakeForTests();
  vi.useRealTimers();
});

describe("WI-2324: focus edges against a live stream", () => {
  it("neither reconnects nor re-reads the list on repeated focus", async () => {
    store.startEventStream();
    open();
    for (let i = 0; i < 6; i++) {
      await vi.advanceTimersByTimeAsync(3_000);
      heartbeat();
      await edge("focus");
    }
    expect(mocks.streams).toHaveLength(1);
    expect(last().unsubscribe).not.toHaveBeenCalled();
    expect(mocks.listSessions).not.toHaveBeenCalled();
  });

  it("does not re-read the list on a short visibility return", async () => {
    store.startEventStream();
    open();
    await edge("visibility", 4_000);
    expect(mocks.streams).toHaveLength(1);
    expect(mocks.listSessions).not.toHaveBeenCalled();
  });

  it("re-reads the list once on a long visibility return, keeping the stream", async () => {
    store.startEventStream();
    open();
    heartbeat();
    await edge("visibility", store.RECONCILE_AFTER_HIDDEN_MS);
    expect(mocks.streams).toHaveLength(1);
    expect(last().unsubscribe).not.toHaveBeenCalled();
    expect(mocks.listSessions).toHaveBeenCalledOnce();
  });

  it("still re-reads the list on an app resume", async () => {
    store.startEventStream();
    open();
    await edge("resume");
    expect(mocks.listSessions).toHaveBeenCalledOnce();
  });
});

describe("WI-2324: focus edges against a connect still in flight", () => {
  it("leaves a pending connect alone and reads nothing", async () => {
    store.startEventStream();
    // No open: the browser is holding this request.
    for (let i = 0; i < 5; i++) {
      await vi.advanceTimersByTimeAsync(3_000);
      await edge("focus");
    }
    expect(mocks.streams).toHaveLength(1);
    expect(last().unsubscribe).not.toHaveBeenCalled();
    expect(mocks.listSessions).not.toHaveBeenCalled();
  });

  it("drops a connect that never answers and sends another", async () => {
    store.startEventStream();
    await vi.advanceTimersByTimeAsync(store.EVENT_STREAM_STALE_MS - 1);
    expect(mocks.streams).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(mocks.streams).toHaveLength(2);
    expect(mocks.streams[0]?.unsubscribe).toHaveBeenCalledOnce();
  });

  it("reconnects on focus once a connect has waited past the quiet window", async () => {
    store.startEventStream();
    await vi.advanceTimersByTimeAsync(store.EVENT_STREAM_QUIET_MS);
    await edge("focus");
    expect(mocks.streams).toHaveLength(2);
    expect(mocks.streams[0]?.unsubscribe).toHaveBeenCalledOnce();
  });
});

describe("WI-2324: a stream that has really gone quiet", () => {
  it("is reconnected on focus, and the list is read once by its reopen", async () => {
    store.startEventStream();
    open();
    await vi.advanceTimersByTimeAsync(21_000);
    await edge("focus");
    expect(mocks.streams).toHaveLength(2);
    // The focus wake leaves the read to the reopen…
    expect(mocks.listSessions).not.toHaveBeenCalled();
    open();
    await vi.advanceTimersByTimeAsync(store.STREAM_SETTLE_MS + 250);
    // …which reads the list once, for the events the drop lost.
    expect(mocks.listSessions).toHaveBeenCalledOnce();
  });
});
