// The cache persist wiring (WI-2695) driven through the real `<Terminal>`:
// hidden panes keep persisting, and persist the raw ring (their xterm screen
// lags the cursor while dormant); a pagehide flushes the pending write on the
// main thread; an offset counts as persisted only once its write is acked; a
// woken session's restarted stream replaces the stored record, and an old
// stream's late ack cannot move the new stream's cursor (#1244); a save while
// a replay is still parsing stores the raw ring.
import { cleanup, render } from "@solidjs/testing-library";
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

type Write = { sessionId: string; position: number; payload: unknown; direct: boolean; ack: (result: string) => void; stream: string | undefined };
const writes: Write[] = [];
vi.mock("../terminalCacheOffthread", () => ({
  postTerminalCacheWrite: (
    sessionId: string,
    position: number,
    payload: unknown,
    _spawn: unknown,
    direct = false,
    stream?: string,
  ) => new Promise<string>((ack) => writes.push({ sessionId, position, payload, direct, ack, stream })),
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
  cleanup();
  // Drain the shared replay queue while fake timers are still installed. Its
  // pump re-arms one fake setTimeout per turn; a timer left pending when the
  // clock goes real never fires, and every later test's replay stalls behind
  // it. A second covers the turns a cancelled mid-snapshot pane leaves.
  if (vi.isFakeTimers()) vi.advanceTimersByTime(1000);
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

async function mountAttached() {
  fakeVogt();
  vi.useFakeTimers();
  const [parked, setParked] = createSignal(false);
  render(() => <Terminal sessionId="t1" parked={parked()} />);
  await vi.advanceTimersByTimeAsync(0);
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
  it("a save while the snapshot is still replaying stores the raw ring", async () => {
    fakeVogt();
    vi.useFakeTimers();
    render(() => <Terminal sessionId="t2" />);
    await vi.advanceTimersByTimeAsync(50);
    const socket = sockets.at(-1)!;
    socket.text({ type: "snapshot-start", scrollback_pos: 8, scrollback_bytes: 8 });
    socket.bytes(8);
    // No snapshot-done yet: the page closes mid-replay.
    window.dispatchEvent(new Event("pagehide"));
    const mid = writes.at(-1)!;
    expect(mid.position).toBe(8);
    expect(mid.payload).toHaveProperty("data");
  });

  it("a hidden pane keeps persisting, as raw bytes at the advanced cursor", async () => {
    const { socket, setParked } = await mountAttached();
    // The snapshot-done persist: serialized screen at 10.
    expect(writes.at(-1)).toMatchObject({ position: 10, payload: { serialized: "SCREEN" } });
    writes.at(-1)!.ack("written");
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
    hidden.ack("written");
    await vi.advanceTimersByTimeAsync(0);

    socket.bytes(7);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(writes.at(-1)).toMatchObject({ position: 22 });
  });

  it("pagehide flushes a pending write directly, before the 5 s cadence", async () => {
    const { socket } = await mountAttached();
    writes.at(-1)!.ack("written");
    await vi.advanceTimersByTimeAsync(0);
    const before = writes.length;
    socket.bytes(4);
    window.dispatchEvent(new Event("pagehide"));
    expect(writes.length).toBe(before + 1);
    expect(writes.at(-1)).toMatchObject({ position: 14, direct: true });
    // The cancelled timer does not post a second copy.
    writes.at(-1)!.ack("written");
    await vi.advanceTimersByTimeAsync(10_000);
    expect(writes.length).toBe(before + 1);
  });

  it("a failed write is not marked persisted and is retried", async () => {
    const { socket } = await mountAttached();
    writes.at(-1)!.ack("written");
    await vi.advanceTimersByTimeAsync(0);
    socket.bytes(3);
    await vi.advanceTimersByTimeAsync(10_000);
    const first = writes.at(-1)!;
    expect(first.position).toBe(13);
    // While unacknowledged, more output does not post the same cursor twice.
    const count = writes.length;
    first.ack("failed");
    await vi.advanceTimersByTimeAsync(10_000);
    expect(writes.length).toBe(count + 1);
    expect(writes.at(-1)!.position).toBe(13);
  });

  it("a stale result for an unchanged stream counts as saved, not retried", async () => {
    // B2 ordering seen from the pane: another writer (a tab, pre-warm) already
    // stored a later cursor, so this pane's write lost the check.
    const { socket } = await mountAttached();
    writes.at(-1)!.ack("written");
    await vi.advanceTimersByTimeAsync(0);
    socket.bytes(2);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(writes.at(-1)).toMatchObject({ position: 12, stream: undefined });
    const count = writes.length;
    writes.at(-1)!.ack("stale");
    await vi.advanceTimersByTimeAsync(10_000);
    expect(writes.length).toBe(count);
  });

  it("a session woken from hibernation replaces the stored record at its restarted offsets", async () => {
    const { socket } = await mountAttached();
    writes.at(-1)!.ack("written");
    await vi.advanceTimersByTimeAsync(0);
    socket.bytes(40);
    await vi.advanceTimersByTimeAsync(10_000);
    writes.at(-1)!.ack("written"); // stored at 50
    await vi.advanceTimersByTimeAsync(0);

    // The engine reports a restart: the new process's snapshot ends at 6,
    // below the 50 already stored.
    socket.text({ type: "snapshot-start", scrollback_pos: 6, scrollback_bytes: 6 });
    socket.bytes(6);
    socket.text({ type: "snapshot-done" });
    await vi.advanceTimersByTimeAsync(50);
    const woken = writes.at(-1)!;
    expect(woken).toMatchObject({ position: 6, stream: expect.any(String) });
    woken.ack("written");
    await vi.advanceTimersByTimeAsync(0);
    // Once replaced, later saves are ordinary ones again.
    socket.bytes(3);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(writes.at(-1)).toMatchObject({ position: 9, stream: undefined });
  });

  // Restart the stream: the engine's new process sends a snapshot ending at 6.
  async function restartAt6(socket: FakeSocket) {
    socket.text({ type: "snapshot-start", scrollback_pos: 6, scrollback_bytes: 6 });
    socket.bytes(6);
    socket.text({ type: "snapshot-done" });
    await vi.advanceTimersByTimeAsync(50);
  }

  it("an old-stream write acked after a restart does not reset the new stream's cursor", async () => {
    const { socket } = await mountAttached();
    writes.at(-1)!.ack("written"); // stored at 10
    await vi.advanceTimersByTimeAsync(0);
    socket.bytes(40);
    await vi.advanceTimersByTimeAsync(10_000);
    const old = writes.at(-1)!;
    expect(old).toMatchObject({ position: 50, stream: undefined });

    await restartAt6(socket);
    const woken = writes.at(-1)!;
    expect(woken).toMatchObject({ position: 6, stream: expect.any(String) });
    woken.ack("written");
    await vi.advanceTimersByTimeAsync(0);
    // The old stream's write resolves last, at a higher offset.
    old.ack("written");
    await vi.advanceTimersByTimeAsync(0);

    // The new stream's next bytes still persist at its own offsets.
    socket.bytes(3);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(writes.at(-1)).toMatchObject({ position: 9, stream: undefined });
  });

  it("a restart is detected while the first write after attach is still in flight", async () => {
    const { socket } = await mountAttached();
    // The write at 10 is posted but not acknowledged.
    expect(writes.at(-1)).toMatchObject({ position: 10 });
    await restartAt6(socket);
    expect(writes.at(-1)).toMatchObject({ position: 6, stream: expect.any(String) });
  });

  it("writes racing while a replace is pending all carry the restarted stream's id", async () => {
    const { socket } = await mountAttached();
    writes.at(-1)!.ack("written");
    await vi.advanceTimersByTimeAsync(0);
    await restartAt6(socket);
    const replacing = writes.at(-1)!;
    expect(replacing.stream).toEqual(expect.any(String));

    // While the replacing write is in flight, more output arrives and the page
    // hides: the pagehide flush is offset-checked against the same stream, so
    // the store keeps whichever of the two is newer, in any commit order.
    socket.bytes(3);
    window.dispatchEvent(new Event("pagehide"));
    const flush = writes.at(-1)!;
    expect(flush).toMatchObject({ position: 9, direct: true, stream: replacing.stream });

    // The flush commits first; the older replacing write then reports stale,
    // which still counts as this stream being stored.
    flush.ack("written");
    await vi.advanceTimersByTimeAsync(0);
    replacing.ack("stale");
    await vi.advanceTimersByTimeAsync(0);
    socket.bytes(2);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(writes.at(-1)).toMatchObject({ position: 11, stream: undefined });
  });

  it("a save while the resume replay is still parsing stores the raw ring", async () => {
    const { socket, setParked } = await mountAttached();
    writes.at(-1)!.ack("written");
    await vi.advanceTimersByTimeAsync(0);
    setParked(true);
    await vi.advanceTimersByTimeAsync(0);
    socket.bytes(5);
    // Back to visible and immediately close: the dormant bytes are queued
    // for replay but not yet parsed.
    setParked(false);
    window.dispatchEvent(new Event("pagehide"));
    const closing = writes.at(-1)!;
    expect(closing.position).toBe(15);
    expect(closing.payload).toHaveProperty("data");
  });


});
