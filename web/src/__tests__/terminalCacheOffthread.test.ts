import "fake-indexeddb/auto";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { loadTerminalCache } from "../terminalCache";
import { handleTerminalCacheWrite } from "../terminalCacheWorker";
import { postTerminalCacheWrite, resetTerminalCacheWorker } from "../terminalCacheOffthread";

async function clearDb(): Promise<void> {
  await new Promise<void>((resolve) => {
    const req = indexedDB.deleteDatabase("vogt-terminal-cache");
    req.onsuccess = req.onerror = req.onblocked = () => resolve();
  });
}

describe("terminal cache worker write", () => {
  beforeEach(async () => {
    await clearDb();
    resetTerminalCacheWorker();
  });

  it("the worker handler stores a serialized entry the main thread can load", async () => {
    await handleTerminalCacheWrite({
      sessionId: "s1",
      outputPosition: 12,
      payload: { serialized: "screen" },
    });
    const entry = await loadTerminalCache("s1");
    expect(entry?.serialized).toBe("screen");
    expect(entry?.outputPosition).toBe(12);
  });

  it("posts the write to a worker instead of writing inline", async () => {
    const postMessage = vi.fn();
    resetTerminalCacheWorker();
    postTerminalCacheWrite("s2", 4, { serialized: "via-worker" }, () => ({ postMessage }));
    expect(postMessage).toHaveBeenCalledWith({
      sessionId: "s2",
      outputPosition: 4,
      payload: { serialized: "via-worker" },
    });
    expect(await loadTerminalCache("s2")).toBeNull();
  });

  it("falls back to a main-thread put when no worker can start", async () => {
    resetTerminalCacheWorker();
    const raw = new TextEncoder().encode("raw\n");
    postTerminalCacheWrite("s3", raw.byteLength, { data: raw }, () => null);
    const entry = await loadTerminalCache("s3");
    expect(new TextDecoder().decode(entry!.data)).toBe("raw\n");
  });
});
