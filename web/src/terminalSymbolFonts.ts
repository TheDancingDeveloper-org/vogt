// The terminal's symbol-glyph fallback (WI-76).
//
// An agent CLI draws its chrome with symbols outside what a phone's system
// monospace font carries: Claude Code's "⏵⏵ bypass permissions" footer drew
// two tofu boxes on Android because nothing on the device has U+23F5, and
// Linux DejaVu lacks it too. Three subsets of the Noto symbol fonts
// (OFL 1.1, `public/fonts/OFL-NotoSans.txt`) ship under one family name and
// are declared here, through the CSS Font Loading API rather than a
// stylesheet, so this table is the single source the terminal installs from
// and the test reads. Ranges are disjoint by construction: each code point
// lives in exactly one file, and a page that never draws a symbol never
// fetches one.

export const TERMINAL_SYMBOL_FONT_FAMILY = "Vogt Terminal Symbols";

export interface TerminalSymbolFace {
  /** File under `public/fonts/`, served at `/fonts/<file>`. */
  file: string;
  /** The Noto family the subset was cut from. */
  source: string;
  /** Glyph count, for the reader; the browser goes by `unicodeRange`. */
  glyphs: number;
  /** The exact code points this file carries, as a CSS `unicode-range`. */
  unicodeRange: string;
}

export const TERMINAL_SYMBOL_FACES: readonly TerminalSymbolFace[] = [
  {
    file: "vogt-terminal-symbols-symbols2.woff2",
    source: "Noto Sans Symbols 2",
    glyphs: 890,
    unicodeRange:
      "U+21AF, U+21E6-21F0, U+21F3, U+2316, U+2318, U+231A-231B, U+2324-2328, U+232B, U+237B, U+237D-237F, U+2394, U+23CE-23CF, U+23E9-23EA, U+23ED-23EF, U+23F1-23FF, U+25A0-2609, U+260E-2612, U+2614-2623, U+2630-2637, U+263C, U+2654-2668, U+267F-268F, U+269E-26A1, U+26AA-26AC, U+26BD-26CD, U+26CF-26E1, U+2700-2704, U+2706-2709, U+270B-271C, U+2722-2727, U+2729-274B, U+274D, U+274F-2753, U+2756-2775, U+2794, U+2798-27AF, U+27B1-27BE, U+2800-28FF, U+2981, U+29BF, U+29EB, U+2B00-2B0D, U+2B12-2B2F, U+2B4D-2B73, U+2B76-2B95, U+2B97-2BFD, U+2BFF",
  },
  {
    file: "vogt-terminal-symbols-symbols.woff2",
    source: "Noto Sans Symbols",
    glyphs: 284,
    unicodeRange:
      "U+2190-2199, U+2300-230F, U+2311-2315, U+2317, U+231C-231F, U+2322-2323, U+2329-232A, U+232C-2335, U+237C, U+2380-2393, U+2396-239A, U+23AF, U+23BE-23CD, U+23D0-23DB, U+23E2-23E8, U+260A-260D, U+2613, U+2624-262F, U+2638-263B, U+263D-2653, U+2669-267E, U+2690-269D, U+26A2-26A9, U+26AD-26BC, U+26CE, U+26E2-26FF, U+271D-2721, U+2776-2793, U+2921-2922",
  },
  {
    file: "vogt-terminal-symbols-math.woff2",
    source: "Noto Sans Math",
    glyphs: 484,
    unicodeRange:
      "U+219A-21AE, U+21B0-21E5, U+21F1-21F2, U+21F4-21FF, U+2310, U+2319, U+2320-2321, U+2336-237A, U+2395, U+239B-23AE, U+23B0-23B9, U+23DC-23E1, U+2900-2920, U+2923-2980, U+2982-29BE, U+29C0-29EA, U+29EC-29FF, U+2B0E-2B11, U+2B30-2B4C, U+2BFE",
  },
];

/** Whether `unicodeRange` covers `codePoint`. */
export function faceCovers(face: TerminalSymbolFace, codePoint: number): boolean {
  return face.unicodeRange.split(",").some((part) => {
    const m = /U\+([0-9A-F]+)(?:-([0-9A-F]+))?/i.exec(part.trim());
    if (!m) return false;
    const lo = parseInt(m[1]!, 16);
    const hi = m[2] ? parseInt(m[2], 16) : lo;
    return codePoint >= lo && codePoint <= hi;
  });
}

let installed = false;

/**
 * Register the faces with the document once. Idempotent, and a no-op where
 * the Font Loading API is absent (jsdom): the terminal then simply has no
 * fallback, which is what it had before.
 */
export function installTerminalSymbolFonts(): void {
  if (installed) return;
  if (typeof document === "undefined" || typeof FontFace === "undefined") return;
  installed = true;
  for (const face of TERMINAL_SYMBOL_FACES) {
    document.fonts.add(
      new FontFace(
        TERMINAL_SYMBOL_FONT_FAMILY,
        `url(/fonts/${face.file}) format("woff2")`,
        { unicodeRange: face.unicodeRange, display: "swap" },
      ),
    );
  }
}
