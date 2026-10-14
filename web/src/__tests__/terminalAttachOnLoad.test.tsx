// The load cap (WI-1413) exercised through the real `<Terminal>`, not a copy of
// its decision. A parked pane with no cached screen asks for an attach slot on
// mount; only the first few get one, and a pane that is opened while its
// staggered attach is still waiting must still end up connected.
import { render } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const disposable = { dispose() {} };

vi.mock("@xterm/xterm/css/xterm.css", () => ({}));
vi.mock("@xterm/xterm", () => {
  class FakeTerm {
    options: Record<string, unknown> = {};
    cols = 80;
    rows = 24;
    textarea = document.createElement("textarea");
    parser = { registerOscHandler: () => disposable };
    buffer = { active: { viewportY: 0, baseY: 0 } };
    loadAddon() {}
    onScroll() {
      return disposable;
    }
    onRender() {
      return disposable;
    }
    onTitleChange() {
      return disposable;
    }
    onBell() {
      return disposable;
    }
    onData() {
      return disposable;
    }
    onSelectionChange() {
      return disposable;
    }
    attachCustomKeyEventHandler() {}
    clearSelection() {}
    scrollLines() {}
    paste() {}
    focus() {}
    open(el: HTMLElement) {
      el.appendChild(document.createElement("div"));
    }
    dispose() {}
  }
  return { Terminal: FakeTerm };
});
vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class {
    fit() {}
  },
}));
vi.mock("@xterm/addon-web-links", () => ({ WebLinksAddon: class {} }));
vi.mock("@xterm/addon-search", () => ({
  SearchAddon: class {
    onDidChangeResults() {
      return disposable;
    }
    findNext() {}
    findPrevious() {}
    clearDecorations() {}
  },
}));

import { createSignal } from "solid-js";

import Terminal from "../Terminal";
import { resetInitialAttaches } from "../initialAttachSlots";
import { MAX_INITIAL_PARKED_ATTACHES } from "../terminalAttachBudget";
import { fakeVogt } from "./harness";

/** A socket that records whether anything connected to it. */
class RecordingSocket extends EventTarget {
  url: string;
  readyState = 0;
  binaryType: BinaryType = "blob";
  sent: string[] = [];
  constructor(url: string) {
    super();
    this.url = url;
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
}

const sockets: RecordingSocket[] = [];

beforeEach(() => {
  sockets.length = 0;
  resetInitialAttaches();
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
  vi.useRealTimers();
  resetInitialAttaches();
});

function sessionOf(socket: RecordingSocket): string {
  const match = /\/api\/sessions\/([^/]+)\/attach/.exec(socket.url);
  return match?.[1] ?? socket.url;
}

describe("initial attach on load", () => {
  it("attaches only the cap when 70 parked panes load", async () => {
    fakeVogt();
    vi.useFakeTimers();

    for (let i = 0; i < 70; i += 1) {
      render(() => <Terminal sessionId={`session-${i}`} parked />);
    }
    await vi.advanceTimersByTimeAsync(2_000);

    const attached = new Set(sockets.map(sessionOf));
    expect(attached.size).toBe(MAX_INITIAL_PARKED_ATTACHES);
  });

  it("connects a pane that is opened before its staggered attach fires", async () => {
    fakeVogt();
    vi.useFakeTimers();

    // Three parked panes fill the budget. The third mounts last, so it waits
    // out the longest stagger before attaching.
    const panes = ["first", "second", "third"].map((id) => {
      const [parked, setParked] = createSignal(true);
      render(() => <Terminal sessionId={id} parked={parked()} />);
      return { id, setParked };
    });

    const opened = panes[2];
    if (!opened) throw new Error("the staggered pane was not mounted");
    await vi.advanceTimersByTimeAsync(100);
    opened.setParked(false);
    await vi.advanceTimersByTimeAsync(1_000);

    expect(sockets.filter((s) => sessionOf(s) === "third").length).toBeGreaterThan(0);
  });
});
