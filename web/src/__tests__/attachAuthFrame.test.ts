import { afterEach, describe, expect, it } from "vitest";

import { openAttach, setBase, setToken } from "../api";
import { REPLAY_TAIL_MAX_BYTES } from "../terminalReplay";
import {
  installRuntimeTransport,
  type RuntimeSocket,
  type RuntimeTransport,
} from "../runtimeTransport";

/** A socket that captures what the client sends and lets the test fire `open`. */
class FakeSocket implements RuntimeSocket {
  readyState = 1;
  binaryType: BinaryType = "blob";
  readonly sent: string[] = [];
  private openListeners: ((event: Event) => void)[] = [];

  send(data: string | ArrayBufferLike | Blob | ArrayBufferView): void {
    if (typeof data === "string") this.sent.push(data);
  }
  close(): void {}
  addEventListener(
    type: "open" | "close" | "error" | "message",
    listener: (event: never) => void,
  ): void {
    if (type === "open") this.openListeners.push(listener as (e: Event) => void);
  }
  fireOpen(): void {
    for (const listener of this.openListeners) listener(new Event("open"));
  }
}

let lastSocket: FakeSocket | null = null;

const networkless: RuntimeTransport = {
  request: () => Promise.reject(new Error("no network in this test")),
  openSocket: () => {
    lastSocket = new FakeSocket();
    return lastSocket;
  },
};

function authFrame(): Record<string, unknown> {
  installRuntimeTransport(networkless);
  setBase("http://engine.test");
  setToken("test-token-1234567890abcdef");
  openAttach("11111111-1111-1111-1111-111111111111", undefined);
  return firstSent();
}

function firstSent(): Record<string, unknown> {
  const socket = lastSocket;
  if (!socket) throw new Error("no socket opened");
  socket.fireOpen();
  expect(socket.sent).toHaveLength(1);
  const frame = socket.sent[0];
  if (frame === undefined) throw new Error("no auth frame sent");
  return JSON.parse(frame) as Record<string, unknown>;
}

afterEach(() => {
  lastSocket = null;
});

describe("attach auth frame", () => {
  it("carries the tail hint on a cold attach (no resume_from)", () => {
    const frame = authFrame();
    expect(frame.type).toBe("auth");
    expect(frame.snapshot_tail_bytes).toBe(REPLAY_TAIL_MAX_BYTES);
    // A cold attach has no cursor to resume from.
    expect(frame.resume_from).toBeUndefined();
  });

  it("omits the tail hint and sends the cursor on a warm reattach", () => {
    installRuntimeTransport(networkless);
    setBase("http://engine.test");
    setToken("test-token-1234567890abcdef");
    openAttach("22222222-2222-2222-2222-222222222222", 4096);
    const frame = firstSent();
    expect(frame.type).toBe("auth");
    expect(frame.resume_from).toBe(4096);
    // The tail cap is a cold-attach affordance; a warm reattach must not narrow.
    expect(frame.snapshot_tail_bytes).toBeUndefined();
  });
});
