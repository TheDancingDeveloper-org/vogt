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
import { describe, expect, it, vi } from "vitest";
import { render, waitFor } from "@solidjs/testing-library";

import Terminal from "../Terminal";
import { noteForeground, onWake, type WakeReason } from "../wakeCoordinator";
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
