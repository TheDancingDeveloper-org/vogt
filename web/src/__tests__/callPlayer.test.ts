// The call's playback queue: clips back to back, in order, flushed on demand.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { CallPlayer, type PlayerContext } from "../callPlayer";

class FakeSource {
  buffer: { duration: number } | null = null;
  onended: (() => void) | null = null;
  startedAt: number | null = null;
  stopped = false;
  connect() {}
  start(at: number) {
    this.startedAt = at;
  }
  stop() {
    this.stopped = true;
  }
}

function fakeContext() {
  const sources: FakeSource[] = [];
  const ctx = {
    currentTime: 10,
    state: "running",
    resume: vi.fn(async () => {}),
    // A clip's "duration" is its byte length in tenths of a second.
    decodeAudioData: vi.fn(async (data: ArrayBuffer) => ({ duration: data.byteLength / 10 })),
    createBufferSource: () => {
      const source = new FakeSource();
      sources.push(source);
      return source as unknown as AudioBufferSourceNode;
    },
    destination: {} as AudioNode,
  };
  return { ctx: ctx as unknown as PlayerContext & { currentTime: number }, sources };
}

describe("the call player", () => {
  beforeEach(() => vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] }));
  afterEach(() => vi.useRealTimers());

  it("plays clips back to back in the order they arrived", async () => {
    const { ctx, sources } = fakeContext();
    const started: number[] = [];
    const player = new CallPlayer(ctx, { onStarted: (_, index) => started.push(index) });
    player.enqueue("r", 0, new ArrayBuffer(20)); // 2 s
    player.enqueue("r", 1, new ArrayBuffer(10)); // 1 s
    await vi.runAllTimersAsync();
    expect(sources).toHaveLength(2);
    expect(sources[0]?.startedAt).toBeCloseTo(10.02);
    expect(sources[1]?.startedAt).toBeCloseTo(12.02);
    expect(started).toEqual([0, 1]);
  });

  it("reports the first audio once and the queue running dry", async () => {
    const { ctx, sources } = fakeContext();
    const events: string[] = [];
    const player = new CallPlayer(ctx, {
      onFirstAudio: (id) => events.push(`first:${id}`),
      onIdle: (id) => events.push(`idle:${id}`),
    });
    player.enqueue("r", 0, new ArrayBuffer(10));
    player.enqueue("r", 1, new ArrayBuffer(10));
    await vi.runAllTimersAsync();
    sources[0]?.onended?.();
    expect(events).toEqual(["first:r"]);
    sources[1]?.onended?.();
    expect(events).toEqual(["first:r", "idle:r"]);
    expect(player.busy()).toBe(false);
  });

  it("goes quiet at once on clear and drops what was still decoding", async () => {
    const { ctx, sources } = fakeContext();
    const started: number[] = [];
    const player = new CallPlayer(ctx, { onStarted: (_, i) => started.push(i) });
    player.enqueue("r", 0, new ArrayBuffer(10));
    await vi.runAllTimersAsync();
    player.enqueue("r", 1, new ArrayBuffer(10));
    player.clear();
    await vi.runAllTimersAsync();
    expect(sources[0]?.stopped).toBe(true);
    expect(sources).toHaveLength(1);
    expect(started).toEqual([0]);
    expect(player.busy()).toBe(false);
  });
});
