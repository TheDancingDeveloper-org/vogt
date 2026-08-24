export type TerminalContextMenuAction = "native" | "copy" | "paste";

/**
 * Decide what a right-click on the terminal host should do.
 *
 * Shift+right-click is the conventional escape hatch to the browser's own
 * context menu over an xterm, so it must fall through untouched — the caller
 * skips `preventDefault()` and its custom copy/paste entirely on "native".
 * Otherwise a live selection copies and an empty selection pastes, matching
 * the mobile/desktop "one of two actions on a terminal" convention.
 */
export function terminalContextMenuAction(
  shiftKey: boolean,
  hasSelection: boolean,
): TerminalContextMenuAction {
  if (shiftKey) return "native";
  return hasSelection ? "copy" : "paste";
}
