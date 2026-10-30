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
/** Each delay is spread by up to this fraction either way, so a fleet of
 *  panes that lost their sockets together does not retry in lockstep. */
export const JITTER_FRACTION = 0.3;
/** Automatic attempts per outage before the pane stops and offers "Retry". */
export const MAX_ATTEMPTS = 5;
/** How long after a return to the front the outage hints stay quiet. */
export const RETURN_QUIET_MS = 1_000;

export interface ReconnectSnapshot {
  /** How many reconnect attempts have been scheduled in this outage. */
  attempt: number;
  /** The backoff before the current pending attempt fires, in ms. */
  delayMs: number;
  /**
   * The automatic attempts are spent: schedule nothing and show the explicit
   * "Retry" state. `delayMs` is meaningless when this is set.
   */
  exhausted: boolean;
}

/**
 * What a terminal socket's close means for the pane. The engine's codes
 * (engine/server/src/ws.rs): 4401 refused credential, 4408 auth frame
 * timeout, 4404 no such session, 4503 core outage and an idle close from the
 * server ping (WI-2704) — the last two are transport, not refusals.
 */
export type CloseAction = "auth" | "gone" | "immediate" | "backoff";

export const CLOSE_IDLE_REASON = "idle";

export function classifyClose(code: number, reason = ""): CloseAction {
  if (code === 4401) return "auth";
  if (code === 4404) return "gone";
  if (code === 4503 || reason === CLOSE_IDLE_REASON) return "immediate";
  return "backoff";
}

/** Spread `delayMs` by up to ±JITTER_FRACTION. `random` is in [0, 1). */
export function jitter(delayMs: number, random: () => number = Math.random): number {
  const spread = (random() * 2 - 1) * JITTER_FRACTION;
  return Math.max(0, Math.round(delayMs * (1 + spread)));
}

export class ReconnectTracker {
  constructor(private readonly random: () => number = Math.random) {}

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
   * backoff. Returns the new attempt number and the jittered delay that
   * attempt waits, or `exhausted` once MAX_ATTEMPTS have been spent.
   */
  scheduleAttempt(): ReconnectSnapshot {
    if (this.attempt >= MAX_ATTEMPTS) {
      return { attempt: this.attempt, delayMs: 0, exhausted: true };
    }
    this.attempt += 1;
    const snapshot: ReconnectSnapshot = {
      attempt: this.attempt,
      delayMs: jitter(this.delayMs, this.random),
      exhausted: false,
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

  /**
   * The reader pressed "Retry" after the automatic attempts ran out: start a
   * fresh run of attempts inside the same outage (the marker stays written).
   */
  restartAttempts(): void {
    this.attempt = 0;
    this.delayMs = INITIAL_DELAY_MS;
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

/** The overlay line once the automatic attempts are spent. */
export const RECONNECT_GAVE_UP = "Disconnected: automatic reconnects stopped";

/** The whole overlay line: "Reconnecting (try N, next in Xs)". */
export function formatReconnectStatus(attempt: number, nextInSeconds: number): string {
  const secs = Math.max(0, Math.ceil(nextInSeconds));
  return `Reconnecting (try ${attempt}, next in ${secs}s)`;
}
