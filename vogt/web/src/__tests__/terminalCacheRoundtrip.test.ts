import "fake-indexeddb/auto";
import { beforeEach, describe, expect, it } from "vitest";

import { loadTerminalCache, saveTerminalCache } from "../terminalCache";

// F5 (WI-129) makes the cache hold either a serialized xterm screen (a live
// pane, the fast restore path) or raw bytes (the headless pre-warm path, or a
// pre-F5 entry). loadTerminalCache must return whichever shape was stored, and
// keep reading raw entries so the format change needs no DB version bump.

async function clearDb(): Promise<void> {
  await new Promise<void>((resolve) => {
    const req = indexedDB.deleteDatabase("vogt-terminal-cache");
    req.onsuccess = req.onerror = req.onblocked = () => resolve();
  });
}

describe("terminal cache round-trip", () => {
  beforeEach(clearDb);

  it("stores and returns a serialized entry (the F5 fast path)", async () => {
    await saveTerminalCache("s-serialized", 4242, { serialized: "\x1b[32mrestored\x1b[0m" });
    const entry = await loadTerminalCache("s-serialized");
    expect(entry).not.toBeNull();
    expect(entry?.serialized).toBe("\x1b[32mrestored\x1b[0m");
    expect(entry?.data).toBeUndefined();
    expect(entry?.outputPosition).toBe(4242);
  });

  it("stores and returns a raw entry (pre-warm / fallback / pre-F5)", async () => {
    const bytes = new TextEncoder().encode("line-a\nline-b\n");
    await saveTerminalCache("s-raw", bytes.byteLength, { data: bytes });
    const entry = await loadTerminalCache("s-raw");
    expect(entry).not.toBeNull();
    expect(entry?.serialized).toBeUndefined();
    expect(Array.from(new Uint8Array(entry!.data!))).toEqual(Array.from(bytes));
  });

  it("ground-state-trims a raw entry whose cursor is past its length", async () => {
    // outputPosition beyond the byte length marks a ring that dropped its head:
    // the load path advances past the first newline so replay starts in ground
    // state.
    const bytes = new TextEncoder().encode("partial-escape\nclean-line\n");
    await saveTerminalCache("s-trim", 100_000, { data: bytes });
    const entry = await loadTerminalCache("s-trim");
    expect(Array.from(new Uint8Array(entry!.data!))).toEqual(
      Array.from(new TextEncoder().encode("clean-line\n")),
    );
  });

  it("ignores an entry with neither serialized nor data", async () => {
    // A malformed / foreign entry never crashes the restore; it reads as a cold
    // start.
    const db = await new Promise<IDBDatabase>((resolve, reject) => {
      const req = indexedDB.open("vogt-terminal-cache", 1);
      req.onupgradeneeded = () => {
        req.result.createObjectStore("sessions", { keyPath: "sessionId" })
          .createIndex("updatedAt", "updatedAt");
      };
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error);
    });
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction("sessions", "readwrite");
      tx.objectStore("sessions").put({
        sessionId: "s-bad",
        outputPosition: 10,
        updatedAt: Date.now(),
      });
      tx.oncomplete = () => resolve();
      tx.onerror = () => reject(tx.error);
    });
    db.close();
    expect(await loadTerminalCache("s-bad")).toBeNull();
  });
});
