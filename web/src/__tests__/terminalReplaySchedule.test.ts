import { describe, expect, it, vi } from "vitest";

import {
  REPLAY_SLICE_BYTES,
  REPLAY_TAIL_MAX_BYTES,
  prepareReplayTail,
  replaySlices,
  scheduleReplay,
} from "../terminalReplay";

function transcript(bytes: number, line = "line\n"): Uint8Array {
  const encoded = new TextEncoder().encode(line);
  const result = new Uint8Array(bytes);
  for (let offset = 0; offset < result.length; offset += encoded.length) {
    result.set(encoded.subarray(0, Math.min(encoded.length, result.length - offset)), offset);
  }
  return result;
}

describe("terminal replay budget", () => {
  it.each([0.5, 4, 16])("bounds a %s MiB cache tail", (sizeMiB) => {
    const data = transcript(sizeMiB * 1024 * 1024);
    const prepared = prepareReplayTail(data, data.byteLength + 1);
    expect(prepared.data.byteLength).toBeLessThanOrEqual(REPLAY_TAIL_MAX_BYTES);
    expect(prepared.outputPosition).toBe(data.byteLength + 1);
    expect(prepared.droppedBytes).toBe(data.byteLength - prepared.data.byteLength);
    if (prepared.data.byteLength < data.byteLength) {
      expect(prepared.data[0]).not.toBe(0x0a);
    }
  });

  it("splits replay without changing bytes", () => {
    const data = transcript(REPLAY_TAIL_MAX_BYTES + 123);
    const slices = replaySlices(data);
    const combined = new Uint8Array(data.byteLength);
    let offset = 0;
    for (const slice of slices) {
      combined.set(slice, offset);
      offset += slice.byteLength;
    }
    expect(slices.every((slice) => slice.byteLength <= REPLAY_SLICE_BYTES)).toBe(true);
    expect(combined).toEqual(data);
  });
});

describe("terminal replay scheduling", () => {
  it("gives eight panes fair, interleaved turns", async () => {
    vi.useFakeTimers();
    const turns: string[] = [];
    const handles = Array.from({ length: 8 }, (_, index) =>
      scheduleReplay(
        `session-${index}`,
        [transcript(REPLAY_SLICE_BYTES * 2, `${index}\n`)],
        (chunk, done) => {
          turns.push(String.fromCharCode(chunk[0] ?? 0));
          done();
        },
        { kind: "snapshot" },
      ),
    );
    await vi.runAllTimersAsync();
    await Promise.all(handles.map((handle) => handle.done));
    expect(turns.slice(0, 8)).toEqual(Array.from({ length: 8 }, (_, i) => String(i)));
    vi.useRealTimers();
  });
});
