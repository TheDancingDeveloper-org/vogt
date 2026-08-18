import { describe, expect, it } from "vitest";
import {
  changeTerminalFontSize,
  MAX_TERMINAL_FONT_SIZE,
  MIN_TERMINAL_FONT_SIZE,
  readTerminalFontSize,
} from "../terminalFont";

describe("explicit terminal-only font controls", () => {
  it("persists clamped steps independently of browser zoom", () => {
    expect(changeTerminalFontSize(1)).toBe(14);
    expect(readTerminalFontSize()).toBe(14);
    expect(changeTerminalFontSize(100)).toBe(MAX_TERMINAL_FONT_SIZE);
    expect(changeTerminalFontSize(-100)).toBe(MIN_TERMINAL_FONT_SIZE);
  });
});
