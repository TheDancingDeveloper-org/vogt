// A foreground wake must leave the reader's place in the scrollback alone.
//
// The phone's terminal "kept going unresponsive to touch" (2026-10-01): the
// swipe did scroll, but every wake — the app returning to the front, the
// WebView regaining focus, the event stream noting its own loss — made each
// open terminal jump back to the live tail, and a stream that could not stay
// up produced a wake every 250 ms. A viewport already at the tail stays there
// without help (xterm follows new output by itself), so the wake has no
// business moving one that is not; the jump-to-bottom chip is the way back.
import { Terminal as XTerm } from "@xterm/xterm";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, waitFor } from "@solidjs/testing-library";

import Terminal from "../Terminal";
import {
  cancelPendingWakeForTests,
  noteForeground,
  onWake,
  type WakeReason,
} from "../wakeCoordinator";
import { fakeVogt } from "./harness";

async function mountTerminal(): Promise<void> {
  fakeVogt({ "GET /sessions": { body: { sessions: [], engine: null } } });
  const { container } = render(() => <Terminal sessionId="eng-1" />);
  await waitFor(() => {
    expect(container.querySelector(".terminal-host .xterm")).toBeTruthy();
  });
}

describe("a wake leaves the terminal viewport where the reader put it", () => {
  for (const reason of ["focus", "visibility", "resume", "sse-reconnect"] as const) {
    it(`does not scroll to the bottom on a "${reason}" wake`, async () => {
      await mountTerminal();
      const scrollToBottom = vi.spyOn(XTerm.prototype, "scrollToBottom");
      // Registered after the terminal's own listener, so by the time this
      // one has run the terminal has already handled the same wake.
      const seen: WakeReason[] = [];
      const stop = onWake((wake) => seen.push(wake.reason));
      try {
        // Back-to-back lifecycle wakes fold together (WI-2705); start clean.
        cancelPendingWakeForTests();
        noteForeground(reason);
        await waitFor(() => expect(seen).toEqual([reason]));
        expect(scrollToBottom).not.toHaveBeenCalled();
      } finally {
        stop();
        scrollToBottom.mockRestore();
      }
    });
  }
});

/** A socket that records whether the terminal opened or reopened one, and what
 *  it sent. The terminal reads `WebSocket.OPEN` and friends off the class, so
 *  the stub carries them too. */
class RecordingSocket extends EventTarget {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;
  readyState = 0;
  binaryType: BinaryType = "blob";
  sent: string[] = [];
  constructor(public url: string) {
    super();
    queueMicrotask(() => {
      this.readyState = 1;
      this.dispatchEvent(new Event("open"));
    });
  }
  send(data: string): void {
    this.sent.push(data);
  }
  close(): void {
    this.readyState = 3;
  }
  /** Ids of the watchdog probes the terminal has sent. */
  pingIds(): number[] {
    return this.sent
      .map((data) => JSON.parse(data) as { type: string; id?: number })
      .filter((msg) => msg.type === "ping")
      .map((msg) => msg.id ?? -1);
  }
  /** Answer a probe, as the server does, so the watchdog has nothing pending. */
  answerLastPing(): void {
    const id = this.pingIds().at(-1);
    if (id === undefined) return;
    this.dispatchEvent(
      new MessageEvent("message", { data: JSON.stringify({ type: "pong", id, pos: 0 }) }),
    );
  }
}

describe("the event stream's reconnect wake for a terminal", () => {
  let sockets: RecordingSocket[] = [];
  const first = (): RecordingSocket => {
    const socket = sockets[0];
    if (!socket) throw new Error("the terminal opened no socket");
    return socket;
  };
  beforeEach(() => {
    sockets = [];
    vi.stubGlobal(
      "WebSocket",
      class extends RecordingSocket {
        constructor(url: string) {
          super(url);
          sockets.push(this);
        }
      },
    );
  });
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  /** Fire the reconnect wake and wait until every listener has seen it. */
  async function fireReconnectWake(): Promise<void> {
    const seen: WakeReason[] = [];
    const stop = onWake((wake) => seen.push(wake.reason));
    try {
      noteForeground("sse-reconnect");
      await waitFor(() => expect(seen).toEqual(["sse-reconnect"]));
    } finally {
      stop();
    }
  }

  it("leaves a pane alone while its own socket is open", async () => {
    await mountTerminal();
    await waitFor(() => expect(sockets).toHaveLength(1));
    expect(first().readyState).toBe(WebSocket.OPEN);
    // Settle the watchdog's own probe on connect, so the only probe left to
    // send is one the wake would force.
    await waitFor(() => expect(first().pingIds().length).toBeGreaterThan(0));
    first().answerLastPing();
    const pingsBefore = first().pingIds().length;

    await fireReconnectWake();

    // The terminal's wake handler ran before this listener. It must not force
    // a watchdog probe on a socket that is open, and must not open a second one.
    expect(first().pingIds()).toHaveLength(pingsBefore);
    expect(sockets).toHaveLength(1);
  });

  it("reconnects a pane whose socket has closed", async () => {
    await mountTerminal();
    await waitFor(() => expect(sockets).toHaveLength(1));
    first().readyState = 3;

    await fireReconnectWake();

    // Same wake on a closed socket: the pane reconnects. So the first test's
    // silence comes from the open-socket guard, not from a handler that never ran.
    await waitFor(() => expect(sockets).toHaveLength(2));
  });
});
