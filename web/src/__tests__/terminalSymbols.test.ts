import { describe, expect, it } from "vitest";
import { TERMINAL_FONT_FAMILY } from "../terminalFont";
import {
  TERMINAL_SYMBOL_FACES,
  TERMINAL_SYMBOL_FONT_FAMILY,
  faceCovers,
} from "../terminalSymbolFonts";

// The terminal's symbol fallback (WI-76): an agent CLI's chrome glyphs must
// have a font to come from on a phone whose system monospace lacks them.
// This pins the three halves of the mechanism to each other — the files that
// ship, the faces that name them, and xterm's font list that reaches them.

const shipped = new Set(
  Object.keys(import.meta.glob("../../public/fonts/*")).map((path) =>
    path.slice(path.lastIndexOf("/") + 1),
  ),
);

describe("terminal symbol fallback font", () => {
  it("ships every file a face names, with the licence beside them", () => {
    expect(TERMINAL_SYMBOL_FACES.length).toBeGreaterThanOrEqual(3);
    for (const face of TERMINAL_SYMBOL_FACES) {
      expect(shipped.has(face.file), face.file).toBe(true);
    }
    expect(shipped.has("OFL-NotoSans.txt")).toBe(true);
  });

  it("covers the glyphs agent CLIs draw, each in exactly one file", () => {
    // ⏵ ⏴ ⎿ ✻ ⧉ ⏺ ↵ ⎇ ⚙ ⚠ ● ⠋
    for (const cp of [0x23f5, 0x23f4, 0x23bf, 0x273b, 0x29c9, 0x23fa, 0x21b5, 0x2387, 0x2699, 0x26a0, 0x25cf, 0x280b]) {
      const owners = TERMINAL_SYMBOL_FACES.filter((face) => faceCovers(face, cp));
      expect(owners.length, `U+${cp.toString(16).toUpperCase()}`).toBe(1);
    }
  });

  it("is the last family in xterm's font list, after the monospace generic", () => {
    const families = TERMINAL_FONT_FAMILY.split(",").map((f) => f.trim());
    expect(families.at(-1)).toBe(`"${TERMINAL_SYMBOL_FONT_FAMILY}"`);
    expect(families.indexOf("monospace")).toBe(families.length - 2);
  });
});
