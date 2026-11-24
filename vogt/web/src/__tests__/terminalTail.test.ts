import { describe, expect, it } from "vitest";
import { tailOf } from "../terminalTail";

const encode = (text: string) => btoa(String.fromCharCode(...new TextEncoder().encode(text)));

describe("tailOf", () => {
  it("keeps the words of a TUI prompt drawn with cursor moves (WI-1830)", () => {
    const frame =
      "\x1b[1;1H╭──────╮\x1b[2;1H│\x1b[1CDo\x1b[1Cyou\x1b[1Cwant\x1b[1Cto\x1b[1Cproceed?" +
      "\x1b[3;1H│\x1b[1C❯\x1b[1C1.\x1b[1CYes\x1b[4;1H╰──────╯\x1b[5;1H   \x1b[6;1H · · ·";
    expect(tailOf(encode(frame))).toBe("│ Do you want to proceed?\n│ ❯ 1. Yes");
  });

  it("drops wordless rows and caps the line count", () => {
    const text = Array.from({ length: 10 }, (_, n) => `line ${n}\n\n────`).join("\n");
    expect(tailOf(encode(text), 3)).toBe("line 7\nline 8\nline 9");
  });
});
