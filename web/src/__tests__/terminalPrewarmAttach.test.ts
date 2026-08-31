import { afterEach, describe, expect, it, vi } from "vitest";

// A warm attach must write the captured tail + absolute end position to the
// same cache the restore path reads, then close its socket. Mock the cache so
// the assertion does not depend on jsdom providing IndexedDB.
const { saveTerminalCache } = vi.hoisted(() => ({
  saveTerminalCache: vi.fn().mockResolvedValue(undefined),
}));
vi.mock("../terminalCache", () => ({
  MAX_TERMINAL_CACHE_BYTES: 4 * 1024 * 1024,
  saveTerminalCache,
}));

import { setBase, setToken } from "../api";
import {
  installRuntimeTransport,
  type RuntimeSocket,
  type RuntimeTransport,
} from "../runtimeTransport";
import { warmAttachOnce } from "../terminalPrewarm";

class FakeSocket implements RuntimeSocket {
  readyState = 1;
  binaryType: BinaryType = "blob";
  readonly sent: string[] = [];
  closed = false;
  private listeners: Record<string, ((event: never) => void)[]> = {};

  send(data: string | ArrayBufferLike | Blob | ArrayBufferView): void {
    if (typeof data === "string") this.sent.push(data);
  }
  close(): void {
    this.closed = true;
  }
  addEventListener(type: string, listener: (event: never) => void): void {
    (this.listeners[type] ??= []).push(listener);
  }
  private fire(type: string, event: unknown): void {
    for (const listener of this.listeners[type] ?? [])
      (listener as (e: unknown) => void)(event);
  }
  fireOpen(): void {
    this.fire("open", new Event("open"));
  }
  fireMessage(data: string | ArrayBuffer): void {
    this.fire("message", { data });
  }
  fireClose(): void {
    this.fire("close", new Event("close"));
  }
}

let lastSocket: FakeSocket | null = null;
const transport: RuntimeTransport = {
  request: () => Promise.reject(new Error("no network in this test")),
  openSocket: () => {
    lastSocket = new FakeSocket();
    return lastSocket;
  },
};

afterEach(() => {
  lastSocket = null;
  saveTerminalCache.mockClear();
});

function setup(): FakeSocket {
  installRuntimeTransport(transport);
  setBase("http://engine.test");
  setToken("test-token-1234567890abcdef");
  return lastSocket as unknown as FakeSocket;
}

describe("warmAttachOnce", () => {
  it("caches the tail with its absolute end position and closes the socket", async () => {
    setup();
    const promise = warmAttachOnce("11111111-1111-1111-1111-111111111111");
    const socket = lastSocket!;
    socket.fireOpen();

    // Cold attach: the auth frame carries the tail cap, no resume_from.
    const frame = JSON.parse(socket.sent[0]!) as Record<string, unknown>;
    expect(frame.type).toBe("auth");
    expect(frame.resume_from).toBeUndefined();
    expect(frame.snapshot_tail_bytes).toBeGreaterThan(0);

    const payload = new TextEncoder().encode("hello\nworld\n"); // 12 bytes
    socket.fireMessage(
      JSON.stringify({
        type: "snapshot-start",
        scrollback_pos: 1000,
        scrollback_bytes: payload.byteLength,
        reset: true,
      }),
    );
    socket.fireMessage(payload.buffer);
    socket.fireMessage(JSON.stringify({ type: "snapshot-done" }));

    await expect(promise).resolves.toBe(true);
    expect(saveTerminalCache).toHaveBeenCalledTimes(1);
    const [id, outputPosition, data] = saveTerminalCache.mock.calls[0]!;
    expect(id).toBe("11111111-1111-1111-1111-111111111111");
    // outputPosition is the absolute stream end, so a later open sends it as
    // resume_from and gets a delta instead of a cold snapshot.
    expect(outputPosition).toBe(1000);
    expect(data.byteLength).toBe(payload.byteLength);
    expect(socket.closed).toBe(true);
  });

  it("does not cache an empty session and still closes", async () => {
    setup();
    const promise = warmAttachOnce("22222222-2222-2222-2222-222222222222");
    const socket = lastSocket!;
    socket.fireOpen();
    socket.fireMessage(
      JSON.stringify({
        type: "snapshot-start",
        scrollback_pos: 0,
        scrollback_bytes: 0,
        reset: true,
      }),
    );
    socket.fireMessage(JSON.stringify({ type: "snapshot-done" }));

    await expect(promise).resolves.toBe(false);
    expect(saveTerminalCache).not.toHaveBeenCalled();
    expect(socket.closed).toBe(true);
  });

  it("resolves false and closes when the socket drops mid-snapshot", async () => {
    setup();
    const promise = warmAttachOnce("33333333-3333-3333-3333-333333333333");
    const socket = lastSocket!;
    socket.fireOpen();
    socket.fireMessage(
      JSON.stringify({
        type: "snapshot-start",
        scrollback_pos: 500,
        scrollback_bytes: 100,
        reset: true,
      }),
    );
    socket.fireClose();
    await expect(promise).resolves.toBe(false);
    expect(saveTerminalCache).not.toHaveBeenCalled();
    expect(socket.closed).toBe(true);
  });
});
