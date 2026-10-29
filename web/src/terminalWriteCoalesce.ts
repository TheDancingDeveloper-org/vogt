/**
 * Coalesce live terminal output into one `term.write` per animation frame.
 *
 * WebSocket binary frames otherwise call `term.write` once per frame. A burst
 * of small frames then schedules a parse-and-paint per frame, which shows up
 * as long tasks. Buffering them and flushing once per `requestAnimationFrame`
 * keeps arrival order and gives xterm one contiguous chunk.
 *
 * Replay and dormancy catch-up stay on the replay queue. They already pace
 * writes and wait on xterm's callback; they must not share this buffer or a
 * live flush could land ahead of a snapshot still parsing.
 */

/** Hard cap on bytes held between frames. A flood flushes immediately. */
export const TERMINAL_WRITE_COALESCE_MAX_BYTES = 256 * 1024;

export interface TerminalWriteCoalescer {
  /** Queue one live frame. Empty frames are ignored. */
  push(chunk: Uint8Array): void;
  /**
   * Write everything queued, synchronously, in arrival order. Safe to call
   * when nothing is queued. Cancels a pending animation frame.
   */
  flush(): void;
  /** Drop queued bytes without writing. Used when the pane is torn down. */
  cancel(): void;
  /** Bytes waiting for the next flush. */
  pendingBytes(): number;
}

export interface CoalesceClock {
  requestFrame(callback: () => void): number;
  cancelFrame(handle: number): void;
}

function defaultClock(): CoalesceClock {
  const request =
    globalThis.requestAnimationFrame?.bind(globalThis) ??
    ((callback: () => void) =>
      setTimeout(callback, 16) as unknown as number);
  const cancel =
    globalThis.cancelAnimationFrame?.bind(globalThis) ??
    ((handle: number) => clearTimeout(handle as unknown as ReturnType<typeof setTimeout>));
  return { requestFrame: request, cancelFrame: cancel };
}

export function createTerminalWriteCoalescer(
  write: (chunk: Uint8Array) => void,
  options: { maxBytes?: number; clock?: CoalesceClock } = {},
): TerminalWriteCoalescer {
  const maxBytes = options.maxBytes ?? TERMINAL_WRITE_COALESCE_MAX_BYTES;
  const clock = options.clock ?? defaultClock();
  const pending: Uint8Array[] = [];
  let pendingBytes = 0;
  let frame: number | null = null;

  function flush(): void {
    if (frame !== null) {
      clock.cancelFrame(frame);
      frame = null;
    }
    if (pending.length === 0) return;
    const chunks = pending.splice(0, pending.length);
    pendingBytes = 0;
    let total = 0;
    for (const chunk of chunks) total += chunk.byteLength;
    const merged = new Uint8Array(total);
    let offset = 0;
    for (const chunk of chunks) {
      merged.set(chunk, offset);
      offset += chunk.byteLength;
    }
    write(merged);
  }

  function schedule(): void {
    if (frame !== null) return;
    frame = clock.requestFrame(() => {
      frame = null;
      flush();
    });
  }

  return {
    push(chunk: Uint8Array): void {
      if (chunk.byteLength === 0) return;
      pending.push(chunk);
      pendingBytes += chunk.byteLength;
      if (pendingBytes >= maxBytes) {
        flush();
        return;
      }
      schedule();
    },
    flush,
    cancel(): void {
      if (frame !== null) {
        clock.cancelFrame(frame);
        frame = null;
      }
      pending.length = 0;
      pendingBytes = 0;
    },
    pendingBytes(): number {
      return pendingBytes;
    },
  };
}
