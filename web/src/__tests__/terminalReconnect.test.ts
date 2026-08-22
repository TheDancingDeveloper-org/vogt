import { describe, expect, it } from "vitest";
import {
  formatQueuedBytes,
  formatReconnectStatus,
  ReconnectTracker,
} from "../terminalReconnect";

describe("terminal reconnect state machine", () => {
  it("increments the attempt count across retries in one outage", () => {
    const tracker = new ReconnectTracker();
    expect(tracker.scheduleAttempt().attempt).toBe(1);
    expect(tracker.scheduleAttempt().attempt).toBe(2);
    expect(tracker.scheduleAttempt().attempt).toBe(3);
    expect(tracker.attempts).toBe(3);
  });

  it("backs off exponentially and caps the delay", () => {
    const tracker = new ReconnectTracker();
    expect(tracker.scheduleAttempt().delayMs).toBe(500);
    expect(tracker.scheduleAttempt().delayMs).toBe(1000);
    expect(tracker.scheduleAttempt().delayMs).toBe(2000);
    expect(tracker.scheduleAttempt().delayMs).toBe(4000);
    expect(tracker.scheduleAttempt().delayMs).toBe(8000);
    expect(tracker.scheduleAttempt().delayMs).toBe(8000);
  });

  it("writes the [disconnected] marker once per outage, not per retry", () => {
    const tracker = new ReconnectTracker();
    expect(tracker.beginOutage().writeMarker).toBe(true);
    expect(tracker.beginOutage().writeMarker).toBe(false);
    tracker.scheduleAttempt();
    expect(tracker.beginOutage().writeMarker).toBe(false);
  });

  it("resets attempt count, backoff and marker on a successful reconnect", () => {
    const tracker = new ReconnectTracker();
    tracker.beginOutage();
    tracker.scheduleAttempt();
    tracker.scheduleAttempt();
    tracker.recover();
    expect(tracker.attempts).toBe(0);
    // A fresh outage starts the marker latch and backoff over again.
    expect(tracker.beginOutage().writeMarker).toBe(true);
    expect(tracker.scheduleAttempt()).toEqual({ attempt: 1, delayMs: 500 });
  });

  it("retryNow collapses the wait without resetting the attempt count", () => {
    const tracker = new ReconnectTracker();
    tracker.scheduleAttempt();
    tracker.scheduleAttempt();
    tracker.retryNow();
    expect(tracker.nextDelayMs).toBe(0);
    expect(tracker.attempts).toBe(2);
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
