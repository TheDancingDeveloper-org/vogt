// One live call, end to end on the client: the socket, the microphone and the
// speaker are fakes; the exchange between them is real.
import { describe, expect, it, vi } from "vitest";

import { CallSession, callSocketUrl, type CallDeps, type CallPlayerLike } from "../callSession";
import type { CallPlayerEvents } from "../callPlayer";
import type { CallView } from "../callProtocol";

class FakeSocket {
  readyState = 0;
  binaryType = "blob";
  sent: (string | ArrayBuffer)[] = [];
  private listeners: Record<string, ((event: unknown) => void)[]> = {};
  addEventListener(type: string, cb: (event: unknown) => void) {
    (this.listeners[type] ??= []).push(cb);
  }
  send(data: string | ArrayBuffer) {
    this.sent.push(data);
  }
  close() {
    this.readyState = 3;
  }
  fire(type: string, event: unknown = {}) {
    for (const cb of this.listeners[type] ?? []) cb(event);
  }
  open() {
    this.readyState = 1;
    this.fire("open");
  }
  server(event: Record<string, unknown>) {
    this.fire("message", { data: JSON.stringify(event) });
  }
  serverBinary(bytes: ArrayBuffer) {
    this.fire("message", { data: bytes });
  }
  drop(code = 1006) {
    this.readyState = 3;
    this.fire("close", { code });
  }
  json(): Record<string, unknown>[] {
    return this.sent.filter((d): d is string => typeof d === "string").map((d) => JSON.parse(d));
  }
  binaries(): ArrayBuffer[] {
    return this.sent.filter((d): d is ArrayBuffer => d instanceof ArrayBuffer);
  }
}

class FakePlayer implements CallPlayerLike {
  queued: [string, number, number][] = [];
  cleared = 0;
  constructor(public events: CallPlayerEvents) {}
  enqueue(responseId: string, index: number, bytes: ArrayBuffer) {
    this.queued.push([responseId, index, bytes.byteLength]);
  }
  clear() {
    this.cleared += 1;
  }
}

function harness(over: Partial<CallDeps> = {}) {
  const sockets: FakeSocket[] = [];
  let onFrame: ((frame: ArrayBuffer) => void) | null = null;
  const captureStop = vi.fn();
  let player: FakePlayer | null = null;
  const views: CallView[] = [];
  const timers: (() => void)[] = [];
  const logs: [string, Record<string, unknown>][] = [];
  let now = 1_000;
  const session = new CallSession(
    {
      url: "ws://engine/api/assistant/call",
      token: "tok",
      openSocket: () => {
        const socket = new FakeSocket();
        sockets.push(socket);
        return socket as never;
      },
      startCapture: async (options) => {
        onFrame = options.onFrame;
        return { stop: captureStop };
      },
      createPlayer: (events) => (player = new FakePlayer(events)),
      log: (event, fields) => logs.push([event, fields]),
      now: () => now,
      // Reconnection waits only; the 20 s ping re-arms itself forever.
      setTimer: (fn, ms) => {
        if (ms !== 20_000) timers.push(fn);
        return 0 as unknown as ReturnType<typeof setTimeout>;
      },
      clearTimer: () => {},
      ...over,
    },
    (view) => views.push(view),
  );
  return {
    session,
    sockets,
    socket: () => sockets.at(-1) as FakeSocket,
    frame: (bytes: ArrayBuffer) => onFrame?.(bytes),
    player: () => player as FakePlayer,
    views,
    view: () => views.at(-1) as CallView,
    captureStop,
    timers,
    logs,
    advance: (ms: number) => (now += ms),
  };
}

async function connected() {
  const h = harness();
  await h.session.start();
  h.socket().open();
  h.socket().server({ type: "session.created", protocol: 1, call_id: "c1", sample_rate: 16000, end_of_turn_ms: 700, barge_in_ms: 500 });
  return h;
}

describe("a live call on the client", () => {
  it("authenticates on open and streams the microphone once the call is up", async () => {
    const h = harness();
    await h.session.start();
    h.frame(new ArrayBuffer(640)); // before the socket exists: dropped
    h.socket().open();
    expect(h.socket().json()[0]).toEqual({ type: "auth", token: "tok" });
    h.frame(new ArrayBuffer(640)); // before session.created: dropped
    expect(h.socket().binaries()).toHaveLength(0);
    h.socket().server({ type: "session.created", protocol: 1, call_id: "c", sample_rate: 16000, end_of_turn_ms: 700, barge_in_ms: 500 });
    h.frame(new ArrayBuffer(640));
    expect(h.socket().binaries()).toHaveLength(1);
    expect(h.view().phase).toBe("listening");
  });

  it("queues each clip under the piece announced before it, and reports playback", async () => {
    const h = await connected();
    h.socket().server({ type: "response.created", response_id: "r1" });
    h.socket().server({ type: "response.audio.start", response_id: "r1", index: 0, text: "Hi.", content_type: "audio/wav", bytes: 3 });
    h.socket().serverBinary(new ArrayBuffer(3));
    expect(h.player().queued).toEqual([["r1", 0, 3]]);
    h.player().events.onStarted?.("r1", 0);
    h.player().events.onIdle?.("r1");
    const sent = h.socket().json().map((e) => e.type);
    expect(sent).toContain("output_audio.started");
    expect(sent).toContain("output_audio.idle");
  });

  it("goes quiet the moment the server says to", async () => {
    const h = await connected();
    h.socket().server({ type: "output_audio.clear", response_id: "r1" });
    expect(h.player().cleared).toBe(1);
  });

  it("measures from the end of speech to the first audio heard", async () => {
    const h = await connected();
    h.socket().server({ type: "input_audio_buffer.speech_stopped" });
    h.advance(500);
    h.player().events.onFirstAudio?.("r1");
    const first = h.logs.find(([event]) => event === "call.first_audio");
    expect(first?.[1]).toEqual({ speech_end_to_heard_ms: 1200 });
  });

  it("sends silence while muted, so a turn still ends", async () => {
    const h = await connected();
    h.session.setMuted(true);
    const loud = new Uint8Array(640).fill(7).buffer;
    h.frame(loud);
    const sent = new Uint8Array(h.socket().binaries()[0] as ArrayBuffer);
    expect(sent.every((b) => b === 0)).toBe(true);
  });

  it("resolves a card with a button press up the socket", async () => {
    const h = await connected();
    expect(h.session.resolve("a1", true)).toBe(true);
    expect(h.socket().json()).toContainEqual({ type: "action.resolve", id: "a1", approve: true });
  });

  it("reconnects after a drop, and ends after repeated ones", async () => {
    const h = await connected();
    h.socket().drop();
    expect(h.view().phase).toBe("reconnecting");
    while (h.timers.length) h.timers.shift()?.();
    expect(h.sockets).toHaveLength(2);
    for (let i = 0; i < 3; i += 1) {
      h.socket().open();
      h.socket().drop();
      while (h.timers.length) h.timers.shift()?.();
    }
    expect(h.view().phase).toBe("ended");
    expect(h.captureStop).toHaveBeenCalled();
  });

  it("says why a call was refused", async () => {
    const h = harness();
    await h.session.start();
    h.socket().open();
    h.socket().drop(4409);
    expect(h.view()).toMatchObject({ phase: "ended", error: "Another call is already in progress." });
  });

  it("ends without a call when the microphone is refused", async () => {
    const h = harness({
      startCapture: async () => {
        throw Object.assign(new Error("no"), { name: "NotAllowedError" });
      },
    });
    await h.session.start();
    expect(h.sockets).toHaveLength(0);
    expect(h.view().error).toMatch(/refused/);
  });

  it("hangs up cleanly: socket closed, microphone released", async () => {
    const h = await connected();
    h.session.hangUp();
    expect(h.socket().readyState).toBe(3);
    expect(h.captureStop).toHaveBeenCalled();
    expect(h.view()).toMatchObject({ phase: "ended", error: null });
  });

  it("derives the socket URL from the engine base or this page", () => {
    expect(callSocketUrl("https://vogt.example", { protocol: "http:", host: "x" })).toBe(
      "wss://vogt.example/api/assistant/call",
    );
    expect(callSocketUrl("", { protocol: "http:", host: "localhost:5173" })).toBe(
      "ws://localhost:5173/api/assistant/call",
    );
  });
});
