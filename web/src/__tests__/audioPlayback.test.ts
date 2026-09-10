// Web Audio playback, proven against a fake AudioContext.
//
// The contract that matters is the one the hands-free loop leans on: a clip
// reports `onEnded` only when it reaches its natural end, never after `stop()`.
// Get that wrong and the loop re-opens the mic on top of the turn that halted
// the reply.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { audioContextAvailable, playAudioBlob, primeAudio, resetAudioForTests, suspendAudio } from "../audioPlayback";

class FakeSource {
  buffer: unknown = null;
  onended: (() => void) | null = null;
  connect = vi.fn();
  start = vi.fn();
  stop = vi.fn(() => {
    // A real node fires `ended` after stop() too — which the module must ignore.
    this.onended?.();
  });
  /** Simulate reaching the natural end. */
  finish() {
    this.onended?.();
  }
}

class FakeAudioContext {
  static instances: FakeAudioContext[] = [];
  static lastSource: FakeSource | null = null;
  state = "suspended";
  destination = {};
  resume = vi.fn(async () => {
    this.state = "running";
  });
  suspend = vi.fn(async () => {
    this.state = "suspended";
  });
  decodeAudioData = vi.fn(async (buf: ArrayBuffer) => ({ duration: buf.byteLength / 1000 }));
  createBufferSource() {
    const s = new FakeSource();
    FakeAudioContext.lastSource = s;
    return s;
  }
  constructor() {
    FakeAudioContext.instances.push(this);
  }
}

describe("Web Audio playback", () => {
  beforeEach(() => {
    FakeAudioContext.instances = [];
    FakeAudioContext.lastSource = null;
    vi.stubGlobal("AudioContext", FakeAudioContext);
    resetAudioForTests();
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    resetAudioForTests();
  });

  it("decodes the clip, resumes the context, and starts a source", async () => {
    expect(audioContextAvailable()).toBe(true);
    const onPlaying = vi.fn();
    const blob = new Blob([new Uint8Array(2000)], { type: "audio/wav" });
    const pb = await playAudioBlob(blob, { onPlaying });
    const ctx = FakeAudioContext.instances[0]!;
    expect(ctx.resume).toHaveBeenCalled();
    expect(ctx.decodeAudioData).toHaveBeenCalledTimes(1);
    expect(FakeAudioContext.lastSource?.start).toHaveBeenCalledTimes(1);
    expect(onPlaying).toHaveBeenCalledWith(2);
    expect(pb.duration).toBe(2);
    expect(pb.playing()).toBe(true);
  });

  it("reports ended on the natural end, and never after stop()", async () => {
    const onEnded = vi.fn();
    const pb = await playAudioBlob(new Blob([new Uint8Array(10)]), { onEnded });
    FakeAudioContext.lastSource!.finish();
    expect(onEnded).toHaveBeenCalledTimes(1);
    expect(pb.playing()).toBe(false);

    const onEnded2 = vi.fn();
    const pb2 = await playAudioBlob(new Blob([new Uint8Array(10)]), { onEnded: onEnded2 });
    pb2.stop(); // the fake fires `ended` on stop, like a real node
    expect(onEnded2).not.toHaveBeenCalled();
    expect(pb2.playing()).toBe(false);
    pb2.stop(); // idempotent
  });

  it("does not start when aborted while decoding", async () => {
    const controller = new AbortController();
    controller.abort();
    await expect(
      playAudioBlob(new Blob([new Uint8Array(10)]), { signal: controller.signal }),
    ).rejects.toMatchObject({ name: "AbortError" });
    expect(FakeAudioContext.lastSource).toBeNull();
  });

  it("shares one context and primes it on a gesture", () => {
    primeAudio();
    primeAudio();
    expect(FakeAudioContext.instances).toHaveLength(1);
    expect(FakeAudioContext.instances[0]!.resume).toHaveBeenCalled();
  });

  it("releases the output stream on request, once", async () => {
    // Called when a reply ends or is halted, before the mic re-opens behind
    // it. A second call on an already-suspended context is a no-op.
    await playAudioBlob(new Blob([new Uint8Array(10)]));
    const ctx = FakeAudioContext.instances[0]!;
    expect(ctx.state).toBe("running");
    suspendAudio();
    expect(ctx.suspend).toHaveBeenCalledTimes(1);
    suspendAudio();
    expect(ctx.suspend).toHaveBeenCalledTimes(1);
    expect(() => suspendAudio()).not.toThrow();
  });

  it("is honest when there is no AudioContext at all", () => {
    vi.unstubAllGlobals();
    resetAudioForTests();
    expect(audioContextAvailable()).toBe(false);
    expect(() => primeAudio()).not.toThrow();
  });
});
