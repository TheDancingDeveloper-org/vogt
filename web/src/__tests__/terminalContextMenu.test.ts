import { describe, expect, it } from "vitest";
import { terminalContextMenuAction } from "../terminalContextMenu";

// #354: shift+right-click is the escape hatch to the browser's native context
// menu over an xterm. It must bypass the custom copy/paste handler entirely so
// the caller never calls preventDefault on it.
describe("terminal context-menu decision (#354)", () => {
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
});
