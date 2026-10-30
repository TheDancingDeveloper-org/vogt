import { describe, expect, it } from "vitest";
import {
  classifyClose,
  formatQueuedBytes,
  jitter,
  MAX_ATTEMPTS,
  formatReconnectStatus,
  ReconnectTracker,
} from "../terminalReconnect";

describe("terminal reconnect state machine", () => {
  it("increments the attempt count across retries in one outage", () => {
    const tracker = new ReconnectTracker(() => 0.5);
    expect(tracker.scheduleAttempt().attempt).toBe(1);
    expect(tracker.scheduleAttempt().attempt).toBe(2);
    expect(tracker.scheduleAttempt().attempt).toBe(3);
    expect(tracker.attempts).toBe(3);
  });

  it("backs off exponentially and caps the delay", () => {
    const tracker = new ReconnectTracker(() => 0.5);
    expect(tracker.scheduleAttempt().delayMs).toBe(500);
    expect(tracker.scheduleAttempt().delayMs).toBe(1000);
    expect(tracker.scheduleAttempt().delayMs).toBe(2000);
    expect(tracker.scheduleAttempt().delayMs).toBe(4000);
    expect(tracker.scheduleAttempt().delayMs).toBe(8000);
  });

  it("stops after MAX_ATTEMPTS and restarts only on an explicit Retry", () => {
    const tracker = new ReconnectTracker(() => 0.5);
    for (let i = 1; i <= MAX_ATTEMPTS; i += 1) {
      expect(tracker.scheduleAttempt()).toMatchObject({ attempt: i, exhausted: false });
    }
    expect(tracker.scheduleAttempt()).toMatchObject({ attempt: MAX_ATTEMPTS, exhausted: true });
    expect(tracker.scheduleAttempt().exhausted).toBe(true);
    tracker.restartAttempts();
    expect(tracker.scheduleAttempt()).toEqual({ attempt: 1, delayMs: 500, exhausted: false });
    // Still the same outage: no second marker.
    tracker.beginOutage();
    expect(tracker.beginOutage().writeMarker).toBe(false);
  });

  it("spreads each delay by at most 30% either way", () => {
    expect(jitter(1000, () => 0)).toBe(700);
    expect(jitter(1000, () => 0.5)).toBe(1000);
    expect(jitter(1000, () => 0.999999)).toBe(1300);
    const tracker = new ReconnectTracker(() => 0);
    expect(tracker.scheduleAttempt().delayMs).toBe(350);
    expect(tracker.scheduleAttempt().delayMs).toBe(700);
  });

  it("writes the [disconnected] marker once per outage, not per retry", () => {
    const tracker = new ReconnectTracker(() => 0.5);
    expect(tracker.beginOutage().writeMarker).toBe(true);
    expect(tracker.beginOutage().writeMarker).toBe(false);
    tracker.scheduleAttempt();
    expect(tracker.beginOutage().writeMarker).toBe(false);
  });

  it("resets attempt count, backoff and marker on a successful reconnect", () => {
    const tracker = new ReconnectTracker(() => 0.5);
    tracker.beginOutage();
    tracker.scheduleAttempt();
    tracker.scheduleAttempt();
    tracker.recover();
    expect(tracker.attempts).toBe(0);
    // A fresh outage starts the marker latch and backoff over again.
    expect(tracker.beginOutage().writeMarker).toBe(true);
    expect(tracker.scheduleAttempt()).toEqual({ attempt: 1, delayMs: 500, exhausted: false });
  });

  it("retryNow collapses the wait without resetting the attempt count", () => {
    const tracker = new ReconnectTracker(() => 0.5);
    tracker.scheduleAttempt();
    tracker.scheduleAttempt();
    tracker.retryNow();
    expect(tracker.nextDelayMs).toBe(0);
    expect(tracker.attempts).toBe(2);
  });
});

describe("terminal socket close codes", () => {
  it("maps the engine's codes to what the pane does", () => {
    expect(classifyClose(4401)).toBe("auth");
    expect(classifyClose(4404)).toBe("gone");
    expect(classifyClose(4503)).toBe("immediate");
    expect(classifyClose(1001, "idle")).toBe("immediate");
    expect(classifyClose(4408)).toBe("backoff");
    expect(classifyClose(1006)).toBe("backoff");
    expect(classifyClose(1000)).toBe("backoff");
  });
});

describe("reconnect overlay text", () => {
  it("surfaces the attempt and countdown", () => {
    expect(formatReconnectStatus(1, 1)).toBe("Reconnecting (try 1, next in 1s)");
    expect(formatReconnectStatus(3, 0)).toBe("Reconnecting (try 3, next in 0s)");
  });

  it("surfaces queued bytes only when there are some", () => {
    expect(formatQueuedBytes(0)).toBe("");
    expect(formatQueuedBytes(12)).toBe("12 bytes queued");
    expect(formatQueuedBytes(2048)).toBe("2.0 KiB queued");
  });
});
