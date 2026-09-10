import { createSignal } from "solid-js";

// A process-wide circuit breaker over the shared transport (#681).
//
// A tab holds a long-lived HTTP/2 connection. When that connection goes stale
// — the server sends GOAWAY on a redeploy, or a tunnel resets — every request
// multiplexed over it aborts before it reaches headers. On its own each caller
// then reacts the worst possible way: `fetchWithRetry` fires its idempotent
// GET/HEAD retries (150ms, 300ms apart), the periodic pollers keep firing on
// their interval, and the event stream reconnects. Multiplied across the
// surfaces a tab has open, that is the ~18 req/s storm the bug reports — every
// request aborting, none succeeding, the dead connection never given room to
// be replaced.
//
// The breaker is the shared memory those independent callers lacked. Transport
// failures accumulate here; once enough land in a row the circuit opens for a
// backed-off, jittered window during which `fetchWithRetry` stops retrying and
// the pollers stop polling (they consult `isCircuitOpen`). The one thing that
// keeps probing is the event stream, whose own bounded reconnect backoff lives
// in `store.ts`; when it reconnects it reports liveness here, the circuit
// closes, and the paused readers resume on their next tick. A single dropped
// request is a blip and never crosses the threshold, so ordinary one-off
// recovery keeps its retries — the breaker engages only for a *sustained*
// outage, which is exactly when retrying is counter-productive.

/** Consecutive transport failures before the circuit opens. Below this a blip
 *  keeps its normal retry/recovery; at or above it we are in an outage. */
const FAILURE_THRESHOLD = 4;
/** First open window once the threshold is crossed. */
const BASE_COOLDOWN_MS = 2_000;
/** Cap on the open window, matching the event stream's reconnect ceiling so a
 *  paused poller never waits materially longer than the probe it depends on. */
const MAX_COOLDOWN_MS = 30_000;

const [healthy, setHealthy] = createSignal(true);

/**
 * Reactive: `false` while the breaker is open (a sustained outage is being
 * ridden out), `true` otherwise. Surfaces already show a non-fatal
 * "Disconnected" state off the event stream's `isConnected`; this is the
 * transport's own view of the same condition for anything that wants it.
 */
export const connectionHealthy = healthy;

let consecutiveFailures = 0;
let openUntil = 0;

/** A transport request reached a responding server (any HTTP status counts —
 *  a 5xx still proves the wire is alive). Closes the circuit. */
export function recordTransportSuccess(): void {
  consecutiveFailures = 0;
  if (openUntil !== 0) openUntil = 0;
  if (!healthy()) setHealthy(true);
}

/** A transport request failed at the wire level (never reached a server, or
 *  ran past its deadline against a dead socket). Opens the circuit once these
 *  cross the threshold, for a window that grows with how long the outage runs. */
export function recordTransportFailure(): void {
  consecutiveFailures += 1;
  if (consecutiveFailures < FAILURE_THRESHOLD) return;
  const over = consecutiveFailures - FAILURE_THRESHOLD;
  const base = Math.min(MAX_COOLDOWN_MS, BASE_COOLDOWN_MS * 2 ** over);
  const jitter = Math.random() * base * 0.3;
  openUntil = Date.now() + base + jitter;
  if (healthy()) setHealthy(false);
}

/**
 * True while the breaker is open. `fetchWithRetry` reads this to drop its
 * inner retries, and the periodic pollers read it to skip a tick — so a dead
 * connection is not hammered while the event stream works to replace it.
 */
export function isCircuitOpen(): boolean {
  return Date.now() < openUntil;
}

/** Test-only: return the breaker to its boot state so global counters do not
 *  leak between cases. */
export function _resetConnectionHealth(): void {
  consecutiveFailures = 0;
  openUntil = 0;
  setHealthy(true);
}
