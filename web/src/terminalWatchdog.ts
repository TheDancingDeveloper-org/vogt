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

  notePingSent(id: number, at: number): void {
    this.pending = { id, at };
    this.lastProbeAt = at;
  }

  notePong(id: number, serverPos: number, localPos: number): WatchdogResult {
    if (!this.pending || this.pending.id !== id) return "healthy";
    this.pending = null;
    return serverPos > localPos ? "recycle" : "healthy";
  }

  noteOutput(at: number): void {
    this.lastOutputAt = at;
    // Old engines do not answer the probe. Output after it is still proof of
    // life, so an idle shell does not reconnect in a loop against one.
    if (this.pending && at >= this.pending.at) this.pending = null;
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
    return forceProbe || now - this.lastProbeAt >= WATCHDOG_INTERVAL_MS
      ? "probe"
      : "healthy";
  }

  reset(): void {
    this.pending = null;
    this.lastProbeAt = Number.NEGATIVE_INFINITY;
    this.lastOutputAt = Number.NEGATIVE_INFINITY;
  }
}
