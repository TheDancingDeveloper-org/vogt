import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type * as Store from "../store";
import type * as Wake from "../wakeCoordinator";

// The event stream is replaced by a recorder: each subscribe call is kept so
// a test can open it, fail it, heartbeat it, and see whether the store tore it
// down. The store is module state, so every test stops the stream it started.
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
const fail = () => last().onError();

// The setup file loads the store before this file's mocks apply, so each test
// gets a fresh store and wake coordinator that see the recorder above.
let store: typeof Store;
let wakeModule: typeof Wake;
let startEventStream: typeof Store.startEventStream;
let stopEventStream: typeof Store.stopEventStream;
let isConnected: typeof Store.isConnected;
let noteForeground: typeof Wake.noteForeground;
let stopWakes: (() => void) | undefined;
let wakes: string[];

beforeEach(async () => {
  vi.resetModules();
  vi.useFakeTimers();
  store = await import("../store");
  wakeModule = await import("../wakeCoordinator");
  ({ startEventStream, stopEventStream, isConnected } = store);
  ({ noteForeground } = wakeModule);
  mocks.streams.length = 0;
  mocks.listSessions.mockReset();
  mocks.listSessions.mockResolvedValue({ sessions: [], hidden_count: 0 });
  wakes = [];
  stopWakes = wakeModule.onWake((wake) => wakes.push(wake.reason));
});

afterEach(() => {
  stopEventStream();
  stopWakes?.();
  vi.useRealTimers();
});

describe("A: a failed stream retry does not wake the app", () => {
  it("stays silent through repeated failed retries", async () => {
    startEventStream();
    open();
    fail();
    for (let i = 0; i < 4; i++) {
      await vi.advanceTimersByTimeAsync(40_000);
      fail();
    }
    await vi.advanceTimersByTimeAsync(250);
    expect(wakes).toEqual([]);
  });

  it("wakes once when the stream reopens after a real gap", async () => {
    startEventStream();
    open();
    fail();
    await vi.advanceTimersByTimeAsync(10_000);
    open();
    await vi.advanceTimersByTimeAsync(250);
    expect(wakes).toEqual(["sse-reconnect"]);
  });

  it("wakes once when the stream reopens after a blip, however short", async () => {
    // Events sent during a short drop are lost as surely as in a long one, so
    // the reopen reads the lists again. The failed retries stay silent.
    startEventStream();
    open();
    fail();
    await vi.advanceTimersByTimeAsync(1_000);
    open();
    await vi.advanceTimersByTimeAsync(250);
    expect(wakes).toEqual(["sse-reconnect"]);
  });
});

describe("C: a stream torn down on focus is read again when it reopens", () => {
  it("wakes once when a quiet stream is reopened on focus", async () => {
    startEventStream();
    open();
    await vi.advanceTimersByTimeAsync(21_000);
    noteForeground("focus");
    await vi.advanceTimersByTimeAsync(250);
    // The forced reconnect is a drop too: events sent while it was down are lost.
    open();
    await vi.advanceTimersByTimeAsync(250);
    expect(wakes.filter((reason) => reason === "sse-reconnect")).toEqual(["sse-reconnect"]);
  });
});

describe("B: a healthy event stream is kept on focus and boot", () => {
  it("does not reconnect on focus while the stream is healthy", async () => {
    startEventStream();
    open();
    last().hooks.onHeartbeat?.();
    await vi.advanceTimersByTimeAsync(5_000);
    noteForeground("focus");
    await vi.advanceTimersByTimeAsync(250);
    expect(mocks.streams).toHaveLength(1);
    expect(last().unsubscribe).not.toHaveBeenCalled();
  });

  it("reconnects on focus once the stream has gone quiet", async () => {
    startEventStream();
    open();
    await vi.advanceTimersByTimeAsync(21_000);
    noteForeground("focus");
    await vi.advanceTimersByTimeAsync(250);
    expect(mocks.streams).toHaveLength(2);
    expect(mocks.streams.at(0)?.unsubscribe).toHaveBeenCalledOnce();
  });

  it("never tears down the stream opened at boot", async () => {
    startEventStream();
    open();
    noteForeground("boot");
    await vi.advanceTimersByTimeAsync(250);
    expect(mocks.streams).toHaveLength(1);
    expect(last().unsubscribe).not.toHaveBeenCalled();
  });
});

describe("C: a grace period before the app shows disconnected", () => {
  // Let the initial session list settle first: its answer would otherwise
  // land after the loss and mark the app connected, which is not the case
  // under test.
  it("keeps the header connected for the grace period after a loss", async () => {
    startEventStream();
    open();
    await vi.advanceTimersByTimeAsync(10);
    expect(isConnected()).toBe(true);
    fail();
    await vi.advanceTimersByTimeAsync(2_000);
    expect(isConnected()).toBe(true);
    await vi.advanceTimersByTimeAsync(600);
    expect(isConnected()).toBe(false);
  });

  it("never shows disconnected for a loss that heals inside the grace", async () => {
    startEventStream();
    open();
    await vi.advanceTimersByTimeAsync(10);
    fail();
    await vi.advanceTimersByTimeAsync(1_000);
    open();
    await vi.advanceTimersByTimeAsync(5_000);
    expect(isConnected()).toBe(true);
  });
});
