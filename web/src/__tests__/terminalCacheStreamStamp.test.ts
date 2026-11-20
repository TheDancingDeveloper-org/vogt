import "fake-indexeddb/auto";
import { beforeEach, describe, expect, it } from "vitest";

import { setBase, setToken } from "../api";
import { installRuntimeTransport, type RuntimeSocket } from "../runtimeTransport";
import { loadTerminalCache, saveTerminalCache } from "../terminalCache";
import { warmAttachOnce } from "../terminalPrewarm";

// #1246: after a stream restart, a second writer that never saw the new stream
// (another tab, the pre-warm path) must not overwrite its record, whatever its
// offset. Every writer stamps its record with the stream it knows; a write
// that is not a restart and lacks the stored stream's id is refused.

const SESSION = "22222222-2222-2222-2222-222222222222";
const bytes = (text: string) => new TextEncoder().encode(text);

async function clearDb(): Promise<void> {
  await new Promise<void>((resolve) => {
    const req = indexedDB.deleteDatabase("vogt-terminal-cache");
    req.onsuccess = req.onerror = req.onblocked = () => resolve();
  });
}

class FakeSocket implements RuntimeSocket {
  readyState = 1;
  binaryType: BinaryType = "blob";
  closed = false;
  private listeners: Record<string, ((event: never) => void)[]> = {};
  send(): void {}
  close(): void {
    this.closed = true;
  }
  addEventListener(type: string, listener: (event: never) => void): void {
    (this.listeners[type] ??= []).push(listener);
  }
  fire(type: string, event: unknown): void {
    for (const listener of this.listeners[type] ?? []) (listener as (e: unknown) => void)(event);
  }
}

describe("terminal cache stream stamps (#1246)", () => {
  beforeEach(clearDb);

  it("another tab still on the old stream cannot overwrite a restarted stream's record", async () => {
    // Tab A and tab B both wrote the old stream, id-less.
    expect(await saveTerminalCache(SESSION, 400, { serialized: "old-a" })).toBe("written");
    // Tab A's session wakes: its restart write replaces the record (#1244).
    expect(await saveTerminalCache(SESSION, 6, { serialized: "new-a" }, "s2")).toBe("written");
    // Tab B, never told of the restart, writes the old stream at a higher offset.
    expect(await saveTerminalCache(SESSION, 500, { serialized: "old-b" })).toBe("stale");
    // A tab holding an older restart's id is refused too.
    expect(await saveTerminalCache(SESSION, 900, { serialized: "older" }, undefined, "s1")).toBe("stale");
    let entry = await loadTerminalCache(SESSION);
    expect(entry).toMatchObject({ outputPosition: 6, stream: "s2", serialized: "new-a" });

    // Tab A, its restart acknowledged, keeps stamping s2; offsets still race.
    expect(await saveTerminalCache(SESSION, 9, { serialized: "new-a2" }, undefined, "s2")).toBe("written");
    // A tab that restored the s2 record adopts its id and competes on offset.
    expect(await saveTerminalCache(SESSION, 8, { serialized: "c" }, undefined, "s2")).toBe("stale");
    expect(await saveTerminalCache(SESSION, 12, { serialized: "c2" }, undefined, "s2")).toBe("written");
    entry = await loadTerminalCache(SESSION);
    expect(entry).toMatchObject({ outputPosition: 12, stream: "s2", serialized: "c2" });

    // #1244 unchanged: a further restart still replaces any other stream.
    expect(await saveTerminalCache(SESSION, 3, { serialized: "s3" }, "s3")).toBe("written");
    expect(await loadTerminalCache(SESSION)).toMatchObject({ outputPosition: 3, stream: "s3" });
  });

  it("id-less writes keep the newest-offset rule on an unstamped record", async () => {
    expect(await saveTerminalCache(SESSION, 10, { serialized: "a" })).toBe("written");
    expect(await saveTerminalCache(SESSION, 5, { serialized: "b" })).toBe("stale");
    expect(await saveTerminalCache(SESSION, 20, { serialized: "c" })).toBe("written");
    const entry = await loadTerminalCache(SESSION);
    expect(entry).toMatchObject({ outputPosition: 20, serialized: "c" });
    expect(entry?.stream).toBeUndefined();
  });

  it("a pre-warm snapshot of the old stream cannot overwrite a restarted stream's record", async () => {
    let socket: FakeSocket | null = null;
    installRuntimeTransport({
      request: () => Promise.reject(new Error("no network in this test")),
      openSocket: () => (socket = new FakeSocket()),
    });
    setBase("http://engine.test");
    setToken("test-token-1234567890abcdef");

    expect(await saveTerminalCache(SESSION, 6, { serialized: "restarted" }, "s2")).toBe("written");

    const warm = warmAttachOnce(SESSION);
    const ws = socket as unknown as FakeSocket;
    ws.fire("open", new Event("open"));
    const tail = bytes("old\nstream\n");
    ws.fire("message", { data: JSON.stringify({ type: "snapshot-start", scrollback_pos: 5000, scrollback_bytes: tail.byteLength }) });
    ws.fire("message", { data: tail.buffer });
    ws.fire("message", { data: JSON.stringify({ type: "snapshot-done" }) });
    await expect(warm).resolves.toBe(true);
    expect(ws.closed).toBe(true);

    expect(await loadTerminalCache(SESSION)).toMatchObject({ outputPosition: 6, stream: "s2", serialized: "restarted" });
  });
});
