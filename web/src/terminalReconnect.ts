// The bookkeeping behind the interactive reconnect overlay.
//
// One websocket outage produces a run of retry attempts with exponential
// backoff. The overlay wants three things the raw socket does not hand us:
//   - a monotonic attempt counter ("try N"), reset only on a real recovery;
//   - a "write the [disconnected] marker once, at the start of the outage"
//     latch, so the scrollback is not spammed with a marker per retry;
//   - the backoff delay, surfaced as "next in Xs".
//
// This is deliberately a plain object with no timers, sockets or xterm in it,
// so the state machine can be exercised directly.

const INITIAL_DELAY_MS = 500;
const MAX_DELAY_MS = 8_000;

export interface ReconnectSnapshot {
  /** How many reconnect attempts have been scheduled in this outage. */
  attempt: number;
  /** The backoff before the current pending attempt fires, in ms. */
  delayMs: number;
}

export class ReconnectTracker {
  private attempt = 0;
  private delayMs = INITIAL_DELAY_MS;
  private markerWritten = false;

  /**
   * Record that the live connection dropped. Returns whether the caller should
   * write the `[disconnected]` marker now — true exactly once per outage, so
   * the first drop writes it and subsequent retries within the same outage do
   * not.
   */
  beginOutage(): { writeMarker: boolean } {
    const writeMarker = !this.markerWritten;
    this.markerWritten = true;
    return { writeMarker };
  }

  /**
   * Record that another reconnect attempt is being scheduled and advance the
   * backoff. Returns the new attempt number and the delay that attempt waits.
   */
  scheduleAttempt(): ReconnectSnapshot {
    this.attempt += 1;
    const snapshot: ReconnectSnapshot = {
      attempt: this.attempt,
      delayMs: this.delayMs,
    };
    this.delayMs = Math.min(this.delayMs * 2, MAX_DELAY_MS);
    return snapshot;
  }

  /**
   * Force the next attempt to fire without further backoff (the "Retry now"
   * button). Does not reset the attempt counter — the user is still inside the
   * same outage — but collapses the wait to zero.
   */
  retryNow(): void {
    this.delayMs = 0;
  }

  /** Record a successful (re)connection: the outage is over. */
  recover(): void {
    this.attempt = 0;
    this.delayMs = INITIAL_DELAY_MS;
    this.markerWritten = false;
  }

  get attempts(): number {
    return this.attempt;
  }

  get nextDelayMs(): number {
    return this.delayMs;
  }
}

/** Round a byte count to a short, human "N bytes queued" phrase. */
export function formatQueuedBytes(bytes: number): string {
  if (bytes <= 0) return "";
  if (bytes < 1024) return `${bytes} bytes queued`;
  const kib = bytes / 1024;
  return `${kib >= 10 ? Math.round(kib) : kib.toFixed(1)} KiB queued`;
}

/** The whole overlay line: "Reconnecting (try N, next in Xs)". */
export function formatReconnectStatus(attempt: number, nextInSeconds: number): string {
  const secs = Math.max(0, Math.ceil(nextInSeconds));
  return `Reconnecting (try ${attempt}, next in ${secs}s)`;
}
