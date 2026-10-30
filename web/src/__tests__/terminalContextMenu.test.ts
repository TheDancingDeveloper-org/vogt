import { describe, expect, it } from "vitest";
import { terminalContextMenuAction, terminalWordAt } from "../terminalContextMenu";

// Shift+right-click is the escape hatch to the browser's native context
// menu over an xterm. It must bypass the custom copy/paste handler entirely so
// the caller never calls preventDefault on it.
describe("terminal context-menu decision", () => {
  it("lets shift+right-click through to the native menu, regardless of selection", () => {
    expect(terminalContextMenuAction(true, true)).toBe("native");
    expect(terminalContextMenuAction(true, false)).toBe("native");
  });

  it("copies a live selection on a plain right-click", () => {
    expect(terminalContextMenuAction(false, true)).toBe("copy");
  });

  it("pastes when there is no selection on a plain right-click", () => {
    expect(terminalContextMenuAction(false, false)).toBe("paste");
  });

  it("never pastes on a coarse-pointer long-press; it selects instead", () => {
    expect(terminalContextMenuAction(false, false, true)).toBe("select");
    expect(terminalContextMenuAction(false, true, true)).toBe("copy");
  });
});

describe("terminal word under a long-press", () => {
  it("selects the whitespace-bounded run under the column", () => {
    const line = "$ cat /etc/hosts now";
    expect(terminalWordAt(line, 8)).toEqual([6, 16]);
    expect(terminalWordAt(line, 0)).toEqual([0, 1]);
    expect(terminalWordAt(line, 19)).toEqual([17, 20]);
  });

  it("selects nothing over whitespace or past the line", () => {
    expect(terminalWordAt("a b", 1)).toBeNull();
    expect(terminalWordAt("a b", 9)).toBeNull();
  });
});
