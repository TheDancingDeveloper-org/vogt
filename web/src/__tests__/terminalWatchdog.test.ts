import { describe, expect, it } from "vitest";
import {
  SocketWatchdog,
  WATCHDOG_INTERVAL_MS,
  WATCHDOG_TIMEOUT_MS,
} from "../terminalWatchdog";

describe("terminal socket watchdog", () => {
  it("probes on wake and recycles an unanswered silent socket", () => {
    const watchdog = new SocketWatchdog();
    expect(watchdog.check(0, true)).toBe("probe");
    watchdog.notePingSent(1, 0);
    expect(watchdog.check(WATCHDOG_TIMEOUT_MS - 1)).toBe("healthy");
    expect(watchdog.check(WATCHDOG_TIMEOUT_MS)).toBe("recycle");
  });

  it("treats output as liveness for an older engine without pong", () => {
    const watchdog = new SocketWatchdog();
    watchdog.notePingSent(1, 100);
    watchdog.noteOutput(200);
    expect(watchdog.check(100 + WATCHDOG_TIMEOUT_MS)).toBe("healthy");
  });

  it("recycles when pong proves that output was missed", () => {
    const watchdog = new SocketWatchdog();
    watchdog.notePingSent(7, 1_000);
    expect(watchdog.notePong(7, 42, 41)).toBe("recycle");
  });

  it("does not probe again before the periodic interval", () => {
    const watchdog = new SocketWatchdog();
    watchdog.notePingSent(1, 0);
    watchdog.notePong(1, 0, 0);
    expect(watchdog.check(WATCHDOG_INTERVAL_MS - 1, false)).toBe("healthy");
  });
});
