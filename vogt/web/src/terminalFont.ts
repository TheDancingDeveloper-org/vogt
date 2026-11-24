import { TERMINAL_SYMBOL_FONT_FAMILY } from "./terminalSymbolFonts";

export const DEFAULT_TERMINAL_FONT_SIZE = 13;
/**
 * xterm's font list. Real monospace fonts first, the `monospace` generic,
 * and last the bundled symbol fallback — reached only for a glyph nothing
 * earlier has (WI-76), so it never changes the cell metrics.
 */
export const TERMINAL_FONT_FAMILY =
  `"JetBrainsMono Nerd Font", "JetBrains Mono", "Fira Code", ui-monospace, SFMono-Regular, Menlo, monospace, "${TERMINAL_SYMBOL_FONT_FAMILY}"`;
export const MIN_TERMINAL_FONT_SIZE = 9;
export const MAX_TERMINAL_FONT_SIZE = 24;
export const TERMINAL_FONT_SIZE_STORAGE_KEY = "vogt.terminalFontSize.v1";
export const TERMINAL_FONT_SIZE_EVENT = "vogt:terminal-font-size";

export function clampTerminalFontSize(value: number): number {
  return Math.min(MAX_TERMINAL_FONT_SIZE, Math.max(MIN_TERMINAL_FONT_SIZE, value));
}

export function readTerminalFontSize(): number {
  try {
    const raw = localStorage.getItem(TERMINAL_FONT_SIZE_STORAGE_KEY);
    if (!raw) return DEFAULT_TERMINAL_FONT_SIZE;
    const parsed = Number(raw);
    return Number.isFinite(parsed)
      ? clampTerminalFontSize(parsed)
      : DEFAULT_TERMINAL_FONT_SIZE;
  } catch {
    return DEFAULT_TERMINAL_FONT_SIZE;
  }
}

export function writeTerminalFontSize(fontSize: number): number {
  const next = clampTerminalFontSize(Math.round(fontSize * 2) / 2);
  try {
    localStorage.setItem(TERMINAL_FONT_SIZE_STORAGE_KEY, String(next));
  } catch {
    /* storage can be unavailable in private / locked-down contexts */
  }
  window.dispatchEvent(
    new CustomEvent(TERMINAL_FONT_SIZE_EVENT, { detail: { fontSize: next } }),
  );
  return next;
}

export function changeTerminalFontSize(delta: number): number {
  return writeTerminalFontSize(readTerminalFontSize() + delta);
}

export function resetTerminalFontSize(): number {
  return writeTerminalFontSize(DEFAULT_TERMINAL_FONT_SIZE);
}
