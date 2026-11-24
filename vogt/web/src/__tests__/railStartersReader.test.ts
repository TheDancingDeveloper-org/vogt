// The starters reader grows from the change feed and is shared across tabs
// (railStarters.ts, WI-4532): one full read at start, then the newest page
// when new ids appear, a full read on reconnect, and one reading tab.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  createStartersReader,
  HEARTBEAT_TIMEOUT_MS,
  MISSING_ID_RETRIES,
  STARTERS_DELTA_SIZE,
  type StartersChannel,
  type StartersPage,
} from "../railStarters";

const link = (child: string, starter: string | null) =>
  ({ id: `ses_${child}`, engine_session_id: child, started_by_session: starter }) as never;

/** A BroadcastChannel stand-in: every port sees the others' messages, not its own. */
function bus() {
  const ports: StartersChannel[] = [];
  return () => {
    const port: StartersChannel = {
      onmessage: null,
      postMessage(message) {
        for (const other of ports) {
          if (other !== port) queueMicrotask(() => other.onmessage?.({ data: message }));
        }
      },
    };
    ports.push(port);
    return port;
  };
}

const flush = async () => {
  for (let i = 0; i < 10; i += 1) await Promise.resolve();
};

describe("starters reader", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("reads in full once, then appends from new ids without re-reading on a timer", async () => {
    let rows = [link("w1", "ovs")];
    const fetchPage = vi.fn(async (_offset: number, _limit: number): Promise<StartersPage> => ({
      sessions: rows,
      truncated: false,
    }));
    const maps: ReadonlyMap<string, string>[] = [];
    const reader = createStartersReader({ fetchPage, channel: null, onMap: (m) => maps.push(m) });

    reader.noteSessions(["ovs", "w1"]);
    await vi.advanceTimersByTimeAsync(1000);
    expect(fetchPage).toHaveBeenCalledTimes(1);
    expect(fetchPage.mock.calls[0]).toEqual([0, 500]);
    expect(Object.fromEntries(maps.at(-1)!)).toEqual({ w1: "ovs" });

    // Nothing new: no read, however long the list stays open.
    reader.noteSessions(["ovs", "w1"]);
    await vi.advanceTimersByTimeAsync(120_000);
    expect(fetchPage).toHaveBeenCalledTimes(1);

    // A session started: the newest page is read and merged in.
    rows = [link("w2", "ovs")];
    reader.noteSessions(["ovs", "w1", "w2"]);
    await vi.advanceTimersByTimeAsync(1000);
    expect(fetchPage).toHaveBeenCalledTimes(2);
    expect(fetchPage.mock.calls[1]).toEqual([0, STARTERS_DELTA_SIZE]);
    expect(Object.fromEntries(maps.at(-1)!)).toEqual({ w1: "ovs", w2: "ovs" });
  });

  it("reads in full when the newest page misses a new id, and on reconnect", async () => {
    let full = [link("w1", "ovs")];
    const fetchPage = vi.fn(async (_offset: number, limit: number): Promise<StartersPage> => ({
      sessions: limit === STARTERS_DELTA_SIZE ? [link("other", "ovs")] : full,
      truncated: limit === STARTERS_DELTA_SIZE,
    }));
    const maps: ReadonlyMap<string, string>[] = [];
    const reader = createStartersReader({ fetchPage, channel: null, onMap: (m) => maps.push(m) });
    reader.noteSessions(["w1"]);
    await vi.advanceTimersByTimeAsync(1000);

    full = [link("w1", "ovs"), link("w9", "ovs")];
    // w9 is unknown and not on the newest page: one full read finds it.
    reader.noteSessions(["w1", "w9"]);
    await vi.advanceTimersByTimeAsync(1000);
    expect(fetchPage.mock.calls.map((c) => c[1])).toEqual([500, STARTERS_DELTA_SIZE, 500]);
    expect(maps.at(-1)!.get("w9")).toBe("ovs");

    reader.reconnected();
    await vi.advanceTimersByTimeAsync(1000);
    expect(fetchPage.mock.calls.map((c) => c[1])).toEqual([500, STARTERS_DELTA_SIZE, 500, 500]);
  });

  it("has one reader across two tabs; the other gets the map by broadcast", async () => {
    const port = bus();
    const fetchA = vi.fn(async (): Promise<StartersPage> => ({ sessions: [link("w1", "ovs")], truncated: false }));
    const fetchB = vi.fn(async (): Promise<StartersPage> => ({ sessions: [link("w1", "ovs")], truncated: false }));
    let mapB: ReadonlyMap<string, string> = new Map();
    const tabA = createStartersReader({ fetchPage: fetchA, channel: port(), onMap: () => {} });
    const tabB = createStartersReader({ fetchPage: fetchB, channel: port(), onMap: (m) => (mapB = m) });

    tabA.noteSessions(["w1"]);
    await vi.advanceTimersByTimeAsync(400);
    expect(tabA.role).toBe("leader");
    tabB.noteSessions(["w1"]);
    await flush();
    await vi.advanceTimersByTimeAsync(2000);
    expect(tabB.role).toBe("follower");
    expect(fetchA).toHaveBeenCalledTimes(1);
    expect(fetchB).not.toHaveBeenCalled();
    expect(mapB.get("w1")).toBe("ovs");

    // The leader tab closes: the other takes over and reads.
    tabA.close();
    await flush();
    await vi.advanceTimersByTimeAsync(2000);
    expect(tabB.role).toBe("leader");
    expect(fetchB).toHaveBeenCalledTimes(1);
  });

  it("asks again for a child whose link was written after its session appeared", async () => {
    let delta = [link("w1", "ovs")];
    const fetchPage = vi.fn(async (_offset: number, limit: number): Promise<StartersPage> => ({
      sessions: limit === STARTERS_DELTA_SIZE ? delta : [link("w1", "ovs")],
      truncated: false,
    }));
    const maps: ReadonlyMap<string, string>[] = [];
    const reader = createStartersReader({ fetchPage, channel: null, onMap: (m) => maps.push(m) });
    reader.noteSessions(["w1"]);
    await vi.advanceTimersByTimeAsync(1000);

    // w2 is in the engine's list, but its link is not written yet.
    reader.noteSessions(["w1", "w2"]);
    await vi.advanceTimersByTimeAsync(1000);
    expect(maps.at(-1)!.has("w2")).toBe(false);
    delta = [link("w2", "ovs")];
    await vi.advanceTimersByTimeAsync(1000);
    expect(maps.at(-1)!.get("w2")).toBe("ovs");
    // Found: no further reads.
    const reads = fetchPage.mock.calls.length;
    await vi.advanceTimersByTimeAsync(60_000);
    expect(fetchPage.mock.calls.length).toBe(reads);
  });

  it("gives up on an id that never gets a link", async () => {
    const fetchPage = vi.fn(async (): Promise<StartersPage> => ({ sessions: [], truncated: false }));
    const reader = createStartersReader({ fetchPage, channel: null, onMap: () => {} });
    reader.noteSessions(["w1"]);
    await vi.advanceTimersByTimeAsync(1000);
    reader.noteSessions(["w1", "w3"]);
    await vi.advanceTimersByTimeAsync(60_000);
    // The full read, the first delta, and the bounded retries.
    expect(fetchPage.mock.calls.length).toBe(2 + MISSING_ID_RETRIES);
  });

  it("takes over when the leader goes quiet without saying bye", async () => {
    const port = bus();
    const portA = port();
    const fetchA = vi.fn(async (): Promise<StartersPage> => ({ sessions: [link("w1", "ovs")], truncated: false }));
    const fetchB = vi.fn(async (): Promise<StartersPage> => ({ sessions: [link("w1", "ovs")], truncated: false }));
    const tabA = createStartersReader({ fetchPage: fetchA, channel: portA, onMap: () => {} });
    const tabB = createStartersReader({ fetchPage: fetchB, channel: port(), onMap: () => {} });
    tabA.noteSessions(["w1"]);
    await vi.advanceTimersByTimeAsync(400);
    tabB.noteSessions(["w1"]);
    await flush();
    await vi.advanceTimersByTimeAsync(2000);
    expect(tabB.role).toBe("follower");

    // A live leader keeps its followers: beats arrive well past the timeout.
    await vi.advanceTimersByTimeAsync(HEARTBEAT_TIMEOUT_MS * 3);
    expect(tabB.role).toBe("follower");
    expect(fetchB).not.toHaveBeenCalled();

    // The leader's tab freezes: nothing it posts gets out any more.
    portA.postMessage = () => {};
    await vi.advanceTimersByTimeAsync(HEARTBEAT_TIMEOUT_MS + 2000);
    expect(tabB.role).toBe("leader");
    expect(fetchB).toHaveBeenCalledTimes(1);
  });
});
