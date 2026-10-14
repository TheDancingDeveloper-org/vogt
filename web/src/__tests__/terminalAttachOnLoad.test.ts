import { afterEach, describe, expect, it, vi } from "vitest";

import { setBase, setToken } from "../api";
import {
  acquireInitialAttach,
  resetInitialAttaches,
} from "../initialAttachSlots";
import { MAX_INITIAL_PARKED_ATTACHES } from "../terminalAttachBudget";
import {
  installRuntimeTransport,
  type RuntimeSocket,
  type RuntimeTransport,
} from "../runtimeTransport";

/**
 * What a parked pane does on load: ask for an initial-attach slot and, only if
 * granted, open its socket. This is the decision `Terminal.tsx` makes in its
 * cache-load path, exercised here without mounting xterm — seventy retained
 * tabs must not open seventy sockets.
 */
function parkedPaneLoads(sessionId: string, open: (id: string) => void): boolean {
  const slot = acquireInitialAttach(sessionId);
  if (slot === null) return false;
  open(sessionId);
  return true;
}

class FakeSocket implements RuntimeSocket {
  readyState = 1;
  binaryType: BinaryType = "blob";
  closed = false;
  addEventListener(): void {}
  send(): void {}
  close(): void {
    this.closed = true;
  }
}

const opened: string[] = [];
const transport: RuntimeTransport = {
  request: () => Promise.reject(new Error("no network in this test")),
  openSocket: (url) => {
    opened.push(url);
    return new FakeSocket();
  },
};

afterEach(() => {
  opened.length = 0;
  resetInitialAttaches();
});

describe("initial attach on load", () => {
  it("attaches at most the cap when 70 parked panes load", async () => {
    installRuntimeTransport(transport);
    setBase("http://engine.test");
    setToken("test-token-1234567890abcdef");
    const { openAttach } = await import("../api");

    const attached = Array.from({ length: 70 }, (_, i) =>
      parkedPaneLoads(`session-${i}`, (id) => openAttach(id)),
    );

    expect(attached.filter(Boolean)).toHaveLength(MAX_INITIAL_PARKED_ATTACHES);
    expect(opened).toHaveLength(MAX_INITIAL_PARKED_ATTACHES);
  });

  it("attaches a pane the budget deferred once it is viewed", async () => {
    installRuntimeTransport(transport);
    setBase("http://engine.test");
    setToken("test-token-1234567890abcdef");
    const { openAttach } = await import("../api");

    for (let i = 0; i < 70; i += 1) {
      parkedPaneLoads(`session-${i}`, (id) => openAttach(id));
    }
    expect(opened).toHaveLength(MAX_INITIAL_PARKED_ATTACHES);

    // Viewing a deferred pane is not gated: the reader asked for it.
    openAttach("session-69");
    expect(opened).toHaveLength(MAX_INITIAL_PARKED_ATTACHES + 1);
    expect(opened.at(-1)).toContain("session-69");
  });

  it("does not open a socket for a pane the budget refuses", () => {
    const open = vi.fn();
    for (let i = 0; i < MAX_INITIAL_PARKED_ATTACHES; i += 1) {
      expect(parkedPaneLoads(`kept-${i}`, open)).toBe(true);
    }
    expect(parkedPaneLoads("deferred", open)).toBe(false);
    expect(open).toHaveBeenCalledTimes(MAX_INITIAL_PARKED_ATTACHES);
    expect(open).not.toHaveBeenCalledWith("deferred");
  });
});
