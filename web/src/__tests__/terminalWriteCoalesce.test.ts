import { describe, expect, it } from "vitest";
import {
  TERMINAL_WRITE_COALESCE_MAX_BYTES,
  createTerminalWriteCoalescer,
} from "../terminalWriteCoalesce";

function bytes(text: string): Uint8Array {
  return new TextEncoder().encode(text);
}

function text(chunk: Uint8Array): string {
  return new TextDecoder().decode(chunk);
}

describe("terminal write coalescer", () => {
  it("flushes queued frames once, in arrival order", () => {
    const frames: Array<() => void> = [];
    const written: string[] = [];
    const coalescer = createTerminalWriteCoalescer(
      (chunk) => written.push(text(chunk)),
      {
        clock: {
          requestFrame(callback) {
            frames.push(callback);
            return frames.length;
          },
          cancelFrame() {},
        },
      },
    );
    coalescer.push(bytes("one"));
    coalescer.push(bytes("two"));
    coalescer.push(bytes(""));
    expect(written).toEqual([]);
    expect(frames).toHaveLength(1);
    expect(coalescer.pendingBytes()).toBe(6);
    frames[0]!();
    expect(written).toEqual(["onetwo"]);
    expect(coalescer.pendingBytes()).toBe(0);
  });

  it("flush writes immediately and cancels the pending frame", () => {
    let cancelled = 0;
    const written: string[] = [];
    const coalescer = createTerminalWriteCoalescer(
      (chunk) => written.push(text(chunk)),
      {
        clock: {
          requestFrame() {
            return 7;
          },
          cancelFrame() {
            cancelled += 1;
          },
        },
      },
    );
    coalescer.push(bytes("a"));
    coalescer.push(bytes("b"));
    coalescer.flush();
    coalescer.flush();
    expect(cancelled).toBe(1);
    expect(written).toEqual(["ab"]);
  });

  it("flushes immediately once the byte cap is reached", () => {
    const written: number[] = [];
    const coalescer = createTerminalWriteCoalescer(
      (chunk) => written.push(chunk.byteLength),
      {
        maxBytes: 8,
        clock: {
          requestFrame() {
            return 1;
          },
          cancelFrame() {},
        },
      },
    );
    coalescer.push(new Uint8Array(5));
    expect(written).toEqual([]);
    coalescer.push(new Uint8Array(4));
    expect(written).toEqual([9]);
    expect(coalescer.pendingBytes()).toBe(0);
  });

  it("cancel drops queued bytes without writing", () => {
    const written: string[] = [];
    const coalescer = createTerminalWriteCoalescer(
      (chunk) => written.push(text(chunk)),
      {
        clock: {
          requestFrame() {
            return 1;
          },
          cancelFrame() {},
        },
      },
    );
    coalescer.push(bytes("drop-me"));
    coalescer.cancel();
    coalescer.flush();
    expect(written).toEqual([]);
    expect(coalescer.pendingBytes()).toBe(0);
    expect(TERMINAL_WRITE_COALESCE_MAX_BYTES).toBeGreaterThan(0);
  });
});
