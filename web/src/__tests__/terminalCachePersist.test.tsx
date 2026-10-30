// The cache persist wiring (WI-2695) driven through the real `<Terminal>`:
// hidden panes keep persisting, and persist the raw ring (their xterm screen
// lags the cursor while dormant); a pagehide flushes the pending write on the
// main thread; an offset counts as persisted only once its write is acked.
import { render } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const disposable = { dispose() {} };

vi.mock("@xterm/xterm/css/xterm.css", () => ({}));
vi.mock("@xterm/xterm", () => {
  class FakeTerm {
    options: Record<string, unknown> = {};
    cols = 80;
    rows = 24;
    textarea = document.createElement("textarea");
    parser = { registerOscHandler: () => disposable };
    buffer = { active: { viewportY: 0, baseY: 0 } };
    loadAddon() {}
    onScroll() { return disposable; }
    onRender() { return disposable; }
    onTitleChange() { return disposable; }
    onBell() { return disposable; }
    onData() { return disposable; }
    onSelectionChange() { return disposable; }
    attachCustomKeyEventHandler() {}
    clearSelection() {}
    scrollLines() {}
    scrollToBottom() {}
    paste() {}
    focus() {}
    reset() {}
    resize() {}
    refresh() {}
    write(_data: unknown, cb?: () => void) { cb?.(); }
    open(el: HTMLElement) { el.appendChild(document.createElement("div")); }
    dispose() {}
  }
  return { Terminal: FakeTerm };
});
vi.mock("@xterm/addon-fit", () => ({ FitAddon: class { fit() {} } }));
vi.mock("@xterm/addon-web-links", () => ({ WebLinksAddon: class {} }));
vi.mock("@xterm/addon-serialize", () => ({
  SerializeAddon: class { serialize() { return "SCREEN"; } },
}));
vi.mock("@xterm/addon-search", () => ({
  SearchAddon: class {
    onDidChangeResults() { return disposable; }
    findNext() {}
    findPrevious() {}
    clearDecorations() {}
  },
}));

type Write = { sessionId: string; position: number; payload: unknown; direct: boolean; ack: (ok: boolean) => void };
const writes: Write[] = [];
vi.mock("../terminalCacheOffthread", () => ({
  postTerminalCacheWrite: (sessionId: string, position: number, payload: unknown, _spawn: unknown, direct = false) =>
    new Promise<boolean>((ack) => writes.push({ sessionId, position, payload, direct, ack })),
}));

import Terminal from "../Terminal";
import { resetInitialAttaches } from "../initialAttachSlots";
import { fakeVogt } from "./harness";

class FakeSocket extends EventTarget {
  url: string;
  readyState = 0;
  binaryType: BinaryType = "blob";
  constructor(url: string) {
    super();
    this.url = url;
    sockets.push(this);
    queueMicrotask(() => {
      this.readyState = 1;
      this.dispatchEvent(new Event("open"));
    });
  }
  send() {}
  close() { this.readyState = 3; }
  text(obj: unknown) { this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify(obj) })); }
  bytes(n: number) { this.dispatchEvent(new MessageEvent("message", { data: new Uint8Array(n).fill(65).buffer })); }
}
const sockets: FakeSocket[] = [];

beforeEach(() => {
  writes.length = 0;
  sockets.length = 0;
  resetInitialAttaches();
  vi.stubGlobal("WebSocket", Object.assign(FakeSocket, { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 }));
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

async function mountAttached() {
  fakeVogt();
  vi.useFakeTimers();
  const [parked, setParked] = createSignal(false);
  render(() => <Terminal sessionId="t1" parked={parked()} />);
  await vi.advanceTimersByTimeAsync(50);
  const socket = sockets.at(-1);
  if (!socket) throw new Error("the pane never attached");
  socket.text({ type: "snapshot-start", scrollback_pos: 10, scrollback_bytes: 10 });
  socket.bytes(10);
  socket.text({ type: "snapshot-done" });
  await vi.advanceTimersByTimeAsync(50);
  return { socket, setParked };
}

describe("terminal cache persist wiring", () => {
  it("a hidden pane keeps persisting, as raw bytes at the advanced cursor", async () => {
    const { socket, setParked } = await mountAttached();
    // The snapshot-done persist: serialized screen at 10.
    expect(writes.at(-1)).toMatchObject({ position: 10, payload: { serialized: "SCREEN" } });
    writes.at(-1)!.ack(true);
    await vi.advanceTimersByTimeAsync(0);

    setParked(true);
    await vi.advanceTimersByTimeAsync(0);
    socket.bytes(5);
    await vi.advanceTimersByTimeAsync(10_000);
    const hidden = writes.at(-1)!;
    expect(hidden.position).toBe(15);
    // Not the stale screen: the raw ring, which holds the hidden bytes.
    expect(hidden.payload).toHaveProperty("data");
    expect((hidden.payload as { data: Uint8Array }).data.byteLength).toBe(15);
    hidden.ack(true);
    await vi.advanceTimersByTimeAsync(0);

    socket.bytes(7);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(writes.at(-1)).toMatchObject({ position: 22 });
  });

  it("pagehide flushes a pending write directly, before the 5 s cadence", async () => {
    const { socket } = await mountAttached();
    writes.at(-1)!.ack(true);
    await vi.advanceTimersByTimeAsync(0);
    const before = writes.length;
    socket.bytes(4);
    window.dispatchEvent(new Event("pagehide"));
    expect(writes.length).toBe(before + 1);
    expect(writes.at(-1)).toMatchObject({ position: 14, direct: true });
    // The cancelled timer does not post a second copy.
    writes.at(-1)!.ack(true);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(writes.length).toBe(before + 1);
  });

  it("a failed write is not marked persisted and is retried", async () => {
    const { socket } = await mountAttached();
    writes.at(-1)!.ack(true);
    await vi.advanceTimersByTimeAsync(0);
    socket.bytes(3);
    await vi.advanceTimersByTimeAsync(10_000);
    const first = writes.at(-1)!;
    expect(first.position).toBe(13);
    // While unacknowledged, more output does not post the same cursor twice.
    const count = writes.length;
    first.ack(false);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(writes.length).toBe(count + 1);
    expect(writes.at(-1)!.position).toBe(13);
  });
});
