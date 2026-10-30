export type TerminalContextMenuAction = "native" | "copy" | "paste" | "select";

/**
 * Decide what a right-click on the terminal host should do.
 *
 * Shift+right-click is the conventional escape hatch to the browser's own
 * context menu over an xterm, so it must fall through untouched — the caller
 * skips `preventDefault()` and its custom copy/paste entirely on "native".
 * Otherwise a live selection copies and an empty selection pastes, matching
 * the desktop "one of two actions on a terminal" convention.
 *
 * On a coarse pointer the contextmenu is a long-press, which is a finger
 * resting on the screen, not an intent to paste: with no selection it starts
 * a word selection instead, and pasting needs an explicit chip tap (WI-2707).
 */
export function terminalContextMenuAction(
  shiftKey: boolean,
  hasSelection: boolean,
  coarsePointer = false,
): TerminalContextMenuAction {
  if (shiftKey) return "native";
  if (hasSelection) return "copy";
  return coarsePointer ? "select" : "paste";
}

/**
 * The [start, end) columns of the word under `col` in `line`, or null over
 * whitespace. A "word" is a run of non-space characters, so paths and URLs
 * select whole — the thing a person long-presses a terminal to grab.
 */
export function terminalWordAt(line: string, col: number): [number, number] | null {
  if (col < 0 || col >= line.length || /\s/.test(line[col] ?? " ")) return null;
  let start = col;
  while (start > 0 && !/\s/.test(line[start - 1] ?? " ")) start -= 1;
  let end = col + 1;
  while (end < line.length && !/\s/.test(line[end] ?? " ")) end += 1;
  return [start, end];
}
