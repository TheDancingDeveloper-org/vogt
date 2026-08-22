// #247, bullet 1: the PTY's window title (OSC 0/2) and its bell (BEL) must
// reach the tab — the title relabels it, the bell lights its activity dot.
// xterm's own title/bell events are the seam, so the terminal is mounted over a
// fake xterm whose captured `onTitleChange`/`onBell` callbacks the test fires.
import { describe, expect, it, vi } from "vitest";
import { render, waitFor } from "@solidjs/testing-library";

const titleCbs: ((title: string) => void)[] = [];
const bellCbs: (() => void)[] = [];
const disposable = { dispose() {} };

vi.mock("@xterm/xterm/css/xterm.css", () => ({}));
vi.mock("@xterm/xterm", () => {
  class FakeTerm {
    options: Record<string, unknown> = {};
    cols = 80;
    rows = 24;
    textarea = document.createElement("textarea");
    loadAddon() {}
    onTitleChange(cb: (title: string) => void) {
      titleCbs.push(cb);
      return disposable;
    }
    onBell(cb: () => void) {
      bellCbs.push(cb);
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

import Terminal from "../Terminal";
import { fakeVogt } from "./harness";

describe("#247 — a PTY's title and bell reach the tab", () => {
  it("forwards a non-empty, trimmed window title through onTitle", async () => {
    fakeVogt();
    const onTitle = vi.fn();
    render(() => <Terminal sessionId="eng-1" onTitle={onTitle} />);
    await waitFor(() => expect(titleCbs.length).toBeGreaterThan(0));

    titleCbs[0]!("  claude — building  ");
    expect(onTitle).toHaveBeenCalledWith("claude — building");

    // A blank title is not a name and must not blank the tab label.
    titleCbs[0]!("   ");
    expect(onTitle).toHaveBeenCalledTimes(1);
  });

  it("forwards the bell through onBell", async () => {
    fakeVogt();
    const onBell = vi.fn();
    render(() => <Terminal sessionId="eng-1" onBell={onBell} />);
    await waitFor(() => expect(bellCbs.length).toBeGreaterThan(0));

    bellCbs.at(-1)!();
    expect(onBell).toHaveBeenCalledTimes(1);
  });
});
