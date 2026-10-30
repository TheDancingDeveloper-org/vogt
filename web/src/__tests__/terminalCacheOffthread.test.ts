import "fake-indexeddb/auto";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { loadTerminalCache, saveTerminalCache } from "../terminalCache";
import { handleTerminalCacheWrite, type TerminalCacheWrite } from "../terminalCacheWorker";
import {
  postTerminalCacheWrite,
  resetTerminalCacheWorker,
  type CacheWorker,
} from "../terminalCacheOffthread";

async function clearDb(): Promise<void> {
  await new Promise<void>((resolve) => {
    const req = indexedDB.deleteDatabase("vogt-terminal-cache");
    req.onsuccess = req.onerror = req.onblocked = () => resolve();
  });
}

/** A worker double that holds messages until the test acks or fails them. */
function fakeWorker() {
  const sent: TerminalCacheWrite[] = [];
  const w: CacheWorker = {
    postMessage: (m) => sent.push(m),
    terminate: vi.fn(),
    onmessage: null,
    onerror: null,
    onmessageerror: null,
  };
  return {
    w,
    sent,
    ack: async (index: number) => {
      const ack = await handleTerminalCacheWrite(sent[index]!);
      w.onmessage?.({ data: ack } as MessageEvent);
    },
    fail: () => w.onerror?.(new Event("error")),
  };
}

describe("terminal cache worker write", () => {
  beforeEach(async () => {
    await clearDb();
    resetTerminalCacheWorker();
  });

  it("the worker handler stores a serialized entry the main thread can load", async () => {
    const ack = await handleTerminalCacheWrite({
      id: 1,
      sessionId: "s1",
      outputPosition: 12,
      payload: { serialized: "screen" },
    });
    expect(ack).toEqual({ id: 1, result: "written" });
    const entry = await loadTerminalCache("s1");
    expect(entry?.serialized).toBe("screen");
    expect(entry?.outputPosition).toBe(12);
  });

  it("resolves only once the worker acknowledges the write", async () => {
    const fake = fakeWorker();
    let done = false;
    const p = postTerminalCacheWrite("s2", 4, { serialized: "via-worker" }, () => fake.w).then((r) => {
      done = r === "written";
    });
    expect(fake.sent).toHaveLength(1);
    expect(fake.sent[0]).toMatchObject({ sessionId: "s2", outputPosition: 4 });
    await Promise.resolve();
    expect(done).toBe(false);
    expect(await loadTerminalCache("s2")).toBeNull();
    await fake.ack(0);
    await p;
    expect(done).toBe(true);
  });

  it("falls back to a main-thread put when no worker can start", async () => {
    const raw = new TextEncoder().encode("raw\n");
    expect(await postTerminalCacheWrite("s3", raw.byteLength, { data: raw }, () => null)).toBe("written");
    const entry = await loadTerminalCache("s3");
    expect(new TextDecoder().decode(entry!.data)).toBe("raw\n");
  });

  it("a worker error replays pending writes on the main thread and stops using it", async () => {
    const fake = fakeWorker();
    const first = postTerminalCacheWrite("s4", 3, { serialized: "pending" }, () => fake.w);
    fake.fail();
    expect(await first).toBe("written");
    expect((await loadTerminalCache("s4"))?.serialized).toBe("pending");
    expect(fake.w.terminate).toHaveBeenCalled();
    // Later writes no longer go to the dead worker.
    expect(await postTerminalCacheWrite("s4", 9, { serialized: "later" }, () => fake.w)).toBe("written");
    expect(fake.sent).toHaveLength(1);
    expect((await loadTerminalCache("s4"))?.serialized).toBe("later");
  });

  it("a messageerror also falls back", async () => {
    const fake = fakeWorker();
    const p = postTerminalCacheWrite("s5", 2, { serialized: "x" }, () => fake.w);
    fake.w.onmessageerror?.(new MessageEvent("messageerror"));
    expect(await p).toBe("written");
    expect((await loadTerminalCache("s5"))?.serialized).toBe("x");
  });

  it("a direct write skips the worker and lands on the main thread", async () => {
    const fake = fakeWorker();
    resetTerminalCacheWorker(fake.w);
    expect(await postTerminalCacheWrite("s9", 5, { serialized: "closing" }, () => null, true)).toBe("written");
    expect(fake.sent).toHaveLength(0);
    expect((await loadTerminalCache("s9"))?.serialized).toBe("closing");
  });
});

describe("terminal cache write ordering", () => {
  beforeEach(clearDb);

  it("an older write that commits after a newer one keeps the newer record", async () => {
    expect(await saveTerminalCache("o1", 50, { serialized: "newer" })).toBe("written");
    expect(await saveTerminalCache("o1", 20, { serialized: "older" })).toBe("stale");
    expect(await saveTerminalCache("o1", 50, { serialized: "same" })).toBe("stale");
    // A restarted stream replaces the record even at a lower offset.
    expect(await saveTerminalCache("o1", 3, { serialized: "woken" }, true)).toBe("written");
    expect((await loadTerminalCache("o1"))?.serialized).toBe("woken");
    expect(await saveTerminalCache("o1", 50, { serialized: "newer" })).toBe("written");
    const entry = await loadTerminalCache("o1");
    expect(entry?.serialized).toBe("newer");
    expect(entry?.outputPosition).toBe(50);
  });

  it("a late worker ack after a newer pagehide flush does not regress the record", async () => {
    resetTerminalCacheWorker();
    const fake = fakeWorker();
    const old = postTerminalCacheWrite("o2", 10, { serialized: "old" }, () => fake.w);
    expect(await postTerminalCacheWrite("o2", 30, { serialized: "flush" }, () => fake.w, true)).toBe("written");
    await fake.ack(0);
    expect(await old).toBe("stale");
    expect((await loadTerminalCache("o2"))?.serialized).toBe("flush");
  });

  it("logs a failed write instead of swallowing it", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const open = vi.spyOn(indexedDB, "open").mockImplementation(() => {
      throw new DOMException("full", "QuotaExceededError");
    });
    expect(await saveTerminalCache("o3", 1, { serialized: "x" })).toBe("failed");
    expect(warn).toHaveBeenCalledWith(
      "[vogt] terminal cache write failed",
      expect.objectContaining({ error: expect.stringContaining("QuotaExceededError") }),
    );
    open.mockRestore();
    warn.mockRestore();
  });
});
