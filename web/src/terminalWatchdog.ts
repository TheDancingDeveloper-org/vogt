/** Liveness state for one terminal WebSocket. It owns no timer or socket. */

export const WATCHDOG_INTERVAL_MS = 30_000;
export const WATCHDOG_TIMEOUT_MS = 5_000;

export type WatchdogResult = "healthy" | "probe" | "recycle";

interface PendingPing {
  id: number;
  at: number;
}

export class SocketWatchdog {
  private pending: PendingPing | null = null;
  private lastProbeAt = Number.NEGATIVE_INFINITY;
  private lastOutputAt = Number.NEGATIVE_INFINITY;
  // A single "behind" pong (server position ahead of what the client has
  // rendered) is a suspect, not a verdict: even with the engine answering a
  // probe with its actual sent position, a pong can momentarily precede the
  // very chunks it is ahead of. So the first behind pong arms a prompt confirm
  // probe instead of recycling; only a second behind pong within the timeout,
  // with no output in between, is treated as a real stall. `suspectAt` is when
  // the first behind pong was seen, or null when there is no live suspicion.
  private suspectAt: number | null = null;

  notePingSent(id: number, at: number): void {
    this.pending = { id, at };
    this.lastProbeAt = at;
  }

  notePong(
    id: number,
    serverPos: number,
    localPos: number,
    at: number,
  ): WatchdogResult {
    if (!this.pending || this.pending.id !== id) return "healthy";
    this.pending = null;
    if (serverPos <= localPos) {
      // Caught up (or the engine reported its true sent position): any prior
      // suspicion is cleared.
      this.suspectAt = null;
      return "healthy";
    }
    // Behind. Recycle only on a second behind pong within the timeout window
    // that no output cleared in between (a genuine stall); otherwise arm a
    // suspect and let `check` issue a prompt confirm probe.
    if (this.suspectAt !== null && at - this.suspectAt < WATCHDOG_TIMEOUT_MS) {
      this.suspectAt = null;
      return "recycle";
    }
    this.suspectAt = at;
    return "healthy";
  }

  noteOutput(at: number): void {
    this.lastOutputAt = at;
    // Old engines do not answer the probe. Output after it is still proof of
    // life, so an idle shell does not reconnect in a loop against one.
    if (this.pending && at >= this.pending.at) this.pending = null;
    // Output flowing is proof the stream is live, so a behind-pong suspicion is
    // no longer credible.
    this.suspectAt = null;
  }

  check(now: number, forceProbe = false): WatchdogResult {
    if (this.pending) {
      if (now - this.pending.at < WATCHDOG_TIMEOUT_MS) return "healthy";
      if (this.lastOutputAt >= this.pending.at) {
        this.pending = null;
        return "healthy";
      }
      return "recycle";
    }
    // A live suspect confirms promptly — one more probe now, not a full
    // interval later — so a real stall is caught within the timeout. A suspect
    // the confirm never resolved in time is dropped as stale.
    if (this.suspectAt !== null) {
      if (now - this.suspectAt >= WATCHDOG_TIMEOUT_MS) {
        this.suspectAt = null;
      } else {
        return "probe";
      }
    }
    return forceProbe || now - this.lastProbeAt >= WATCHDOG_INTERVAL_MS
      ? "probe"
      : "healthy";
  }

  reset(): void {
    this.pending = null;
    this.lastProbeAt = Number.NEGATIVE_INFINITY;
    this.lastOutputAt = Number.NEGATIVE_INFINITY;
    this.suspectAt = null;
  }
}
