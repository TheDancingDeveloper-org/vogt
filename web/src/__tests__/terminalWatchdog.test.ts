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

  it("does not recycle on a single behind pong; it arms a confirm probe", () => {
    // F2: one behind pong is a suspect, not a verdict — the pong can momentarily
    // precede the very chunks it is ahead of. No recycle, and check() issues a
    // prompt confirm probe rather than waiting a full interval.
    const watchdog = new SocketWatchdog();
    watchdog.notePingSent(7, 1_000);
    expect(watchdog.notePong(7, 42, 41, 1_010)).toBe("healthy");
    expect(watchdog.check(1_020, false)).toBe("probe");
  });

  it("recycles on a second behind pong within the window with no output", () => {
    const watchdog = new SocketWatchdog();
    watchdog.notePingSent(1, 1_000);
    expect(watchdog.notePong(1, 42, 41, 1_010)).toBe("healthy"); // first: suspect
    watchdog.notePingSent(2, 1_020); // the confirm probe
    // Second behind pong, within WATCHDOG_TIMEOUT_MS of the first, nothing in
    // between: a real stall.
    expect(watchdog.notePong(2, 43, 41, 1_030)).toBe("recycle");
  });

  it("clears the suspicion when output arrives between the two probes", () => {
    const watchdog = new SocketWatchdog();
    watchdog.notePingSent(1, 1_000);
    expect(watchdog.notePong(1, 42, 41, 1_010)).toBe("healthy"); // suspect
    watchdog.noteOutput(1_015); // the stream is live after all
    watchdog.notePingSent(2, 1_020);
    // The next behind pong starts a fresh suspicion, it does not recycle.
    expect(watchdog.notePong(2, 43, 41, 1_030)).toBe("healthy");
  });

  it("clears the suspicion when the confirm pong shows the client caught up", () => {
    const watchdog = new SocketWatchdog();
    watchdog.notePingSent(1, 1_000);
    expect(watchdog.notePong(1, 42, 41, 1_010)).toBe("healthy"); // suspect
    watchdog.notePingSent(2, 1_020);
    expect(watchdog.notePong(2, 42, 42, 1_030)).toBe("healthy"); // caught up
    // Suspicion cleared: the next lone behind pong is again only a suspect.
    watchdog.notePingSent(3, 1_040);
    expect(watchdog.notePong(3, 50, 41, 1_050)).toBe("healthy");
  });

  it("drops a stale suspect the confirm never resolved in time", () => {
    const watchdog = new SocketWatchdog();
    watchdog.notePingSent(1, 1_000);
    expect(watchdog.notePong(1, 42, 41, 1_010)).toBe("healthy"); // suspect at 1010
    // No confirm pong arrives; past the timeout the suspicion is dropped and the
    // watchdog falls back to its ordinary interval cadence.
    expect(watchdog.check(1_010 + WATCHDOG_TIMEOUT_MS, false)).toBe("healthy");
  });

  it("does not probe again before the periodic interval", () => {
    const watchdog = new SocketWatchdog();
    watchdog.notePingSent(1, 0);
    watchdog.notePong(1, 0, 0, 0);
    expect(watchdog.check(WATCHDOG_INTERVAL_MS - 1, false)).toBe("healthy");
  });
});
