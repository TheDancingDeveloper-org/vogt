export type ShortcutContext =
  | "global"
  | "outside-editable"
  | "terminal"
  | "editor"
  | "command-palette";

export interface ShortcutBinding {
  key: string | readonly string[];
  ctrlOrMeta?: boolean;
  ctrl?: boolean;
  meta?: boolean;
  alt?: boolean;
  shift?: boolean;
}

export interface KeyboardShortcut {
  id: string;
  keys: readonly string[];
  description: string;
  category: string;
  context: ShortcutContext;
  contextLabel: string;
  binding?: ShortcutBinding;
}

/**
 * The one inventory used by both the shortcut listener and shortcut help.
 * Surface-local handlers (terminal, editor and palette) are declared here so
 * the help can state where they apply; app-level bindings also carry a
 * machine-readable binding and are dispatched by `matchAppShortcut`.
 */
export const KEYBOARD_SHORTCUTS: readonly KeyboardShortcut[] = [
  {
    id: "open-command-palette",
    keys: ["Ctrl/Cmd", "K"],
    description: "Open command palette",
    category: "Navigation",
    context: "global",
    contextLabel: "Anywhere",
    binding: { key: "k", ctrlOrMeta: true, alt: false, shift: false },
  },
  {
    id: "new-terminal-session",
    keys: ["Ctrl/Cmd", "Shift", "T"],
    description: "New terminal session",
    category: "Navigation",
    context: "global",
    contextLabel: "Anywhere",
    binding: { key: "t", ctrlOrMeta: true, alt: false, shift: true },
  },
  {
    id: "close-active-tab",
    keys: ["Ctrl/Cmd", "Shift", "W"],
    description: "Close active tab",
    category: "Navigation",
    context: "global",
    contextLabel: "Anywhere",
    binding: { key: "w", ctrlOrMeta: true, alt: false, shift: true },
  },
  {
    id: "previous-tab",
    keys: ["Ctrl/Cmd", "Alt", "←"],
    description: "Previous tab",
    category: "Navigation",
    context: "global",
    contextLabel: "Anywhere",
    binding: {
      key: "arrowleft",
      ctrlOrMeta: true,
      alt: true,
      shift: false,
    },
  },
  {
    id: "next-tab",
    keys: ["Ctrl/Cmd", "Alt", "→"],
    description: "Next tab",
    category: "Navigation",
    context: "global",
    contextLabel: "Anywhere",
    binding: {
      key: "arrowright",
      ctrlOrMeta: true,
      alt: true,
      shift: false,
    },
  },
  {
    id: "terminal-copy",
    keys: ["Ctrl", "Shift", "C"],
    description: "Copy selection",
    category: "Terminal",
    context: "terminal",
    contextLabel: "Terminal only",
  },
  {
    id: "terminal-paste",
    keys: ["Ctrl", "Shift", "V"],
    description: "Paste",
    category: "Terminal",
    context: "terminal",
    contextLabel: "Terminal only",
  },
  {
    id: "terminal-select-all",
    keys: ["Ctrl", "Shift", "A"],
    description: "Select all",
    category: "Terminal",
    context: "terminal",
    contextLabel: "Terminal only",
  },
  {
    id: "terminal-search",
    keys: ["Ctrl/Cmd", "Shift", "F"],
    description: "Find in the terminal buffer",
    category: "Terminal",
    context: "terminal",
    contextLabel: "Terminal only",
  },
  {
    id: "terminal-interrupt",
    keys: ["Ctrl", "C"],
    description: "Copy with a selection, otherwise interrupt",
    category: "Terminal",
    context: "terminal",
    contextLabel: "Terminal only",
  },
  {
    id: "terminal-context-menu",
    keys: ["Right-click"],
    description: "Copy or paste",
    category: "Terminal",
    context: "terminal",
    contextLabel: "Terminal only",
  },
  {
    id: "terminal-middle-paste",
    keys: ["Middle-click"],
    description: "Paste on Linux",
    category: "Terminal",
    context: "terminal",
    contextLabel: "Terminal only · Linux",
  },
  {
    id: "editor-save",
    keys: ["Ctrl/Cmd", "S"],
    description: "Save file",
    category: "Editor",
    context: "editor",
    contextLabel: "Editor only",
  },
  {
    id: "editor-find",
    keys: ["Ctrl/Cmd", "F"],
    description: "Find",
    category: "Editor",
    context: "editor",
    contextLabel: "Editor only",
  },
  {
    id: "editor-replace",
    keys: ["Ctrl/Cmd", "H"],
    description: "Find and replace",
    category: "Editor",
    context: "editor",
    contextLabel: "Editor only",
  },
  {
    id: "editor-go-to-line",
    keys: ["Ctrl/Cmd", "G"],
    description: "Go to line",
    category: "Editor",
    context: "editor",
    contextLabel: "Editor only",
  },
  {
    id: "editor-comment",
    keys: ["Ctrl/Cmd", "/"],
    description: "Toggle comment",
    category: "Editor",
    context: "editor",
    contextLabel: "Editor only",
  },
  {
    id: "editor-line-up",
    keys: ["Alt", "↑"],
    description: "Move line up",
    category: "Editor",
    context: "editor",
    contextLabel: "Editor only",
  },
  {
    id: "editor-line-down",
    keys: ["Alt", "↓"],
    description: "Move line down",
    category: "Editor",
    context: "editor",
    contextLabel: "Editor only",
  },
  {
    id: "editor-next-match",
    keys: ["Ctrl/Cmd", "D"],
    description: "Add cursor to next match",
    category: "Editor",
    context: "editor",
    contextLabel: "Editor only",
  },
  {
    id: "palette-previous",
    keys: ["↑"],
    description: "Navigate up",
    category: "Command Palette",
    context: "command-palette",
    contextLabel: "Palette only",
  },
  {
    id: "palette-next",
    keys: ["↓"],
    description: "Navigate down",
    category: "Command Palette",
    context: "command-palette",
    contextLabel: "Palette only",
  },
  {
    id: "palette-execute",
    keys: ["Enter"],
    description: "Execute command",
    category: "Command Palette",
    context: "command-palette",
    contextLabel: "Palette only",
  },
  {
    id: "palette-close",
    keys: ["Esc"],
    description: "Close palette",
    category: "Command Palette",
    context: "command-palette",
    contextLabel: "Palette only",
  },
  {
    id: "board-open-item",
    keys: ["Enter / Space"],
    description: "Open the focused card (a single click of the card body does the same)",
    category: "Board",
    context: "outside-editable",
    contextLabel: "Board · a card is focused",
  },
  {
    id: "board-focus-columns",
    keys: ["← / →"],
    description: "Move focus across columns",
    category: "Board",
    context: "outside-editable",
    contextLabel: "Board · a card is focused",
  },
  {
    id: "board-focus-cards",
    keys: ["↑ / ↓"],
    description: "Move focus within a column",
    category: "Board",
    context: "outside-editable",
    contextLabel: "Board · a card is focused",
  },
  {
    id: "board-propose-move",
    keys: ["Shift", "← / →"],
    description: "Propose a move (same reason prompt as a drop)",
    category: "Board",
    context: "outside-editable",
    contextLabel: "Board · a card is focused",
  },
  {
    id: "board-quick-create",
    keys: ["n"],
    description: "Raise a work item without leaving the board",
    category: "Board",
    context: "outside-editable",
    contextLabel: "Board",
  },
  {
    id: "show-shortcut-help",
    keys: ["?"],
    description: "Show this help",
    category: "Help",
    context: "outside-editable",
    contextLabel: "Outside text fields, editors, and terminals",
    // `?` is Shift+/ on common layouts. Match the produced character and do
    // not require a particular Shift state so other layouts work too.
    binding: { key: "?", ctrl: false, meta: false, alt: false },
  },
  {
    id: "toggle-places-rail",
    keys: ["Ctrl/Cmd", "B"],
    description: "Show or hide the Places rail",
    category: "Navigation",
    context: "global",
    contextLabel: "Anywhere",
    binding: { key: "b", ctrlOrMeta: true, alt: false, shift: false },
  },
  {
    id: "inbox-next-entry",
    keys: ["j"],
    description: "Focus the next Inbox entry",
    category: "Inbox",
    context: "outside-editable",
    contextLabel: "Inbox",
  },
  {
    id: "inbox-previous-entry",
    keys: ["k"],
    description: "Focus the previous Inbox entry",
    category: "Inbox",
    context: "outside-editable",
    contextLabel: "Inbox",
  },
  {
    id: "inbox-archive-entry",
    keys: ["e"],
    description: "Archive the focused Inbox entry",
    category: "Inbox",
    context: "outside-editable",
    contextLabel: "Inbox",
  },
  {
    id: "inbox-snooze-entry",
    keys: ["s"],
    description: "Snooze the focused Inbox entry",
    category: "Inbox",
    context: "outside-editable",
    contextLabel: "Inbox",
  },
  {
    id: "inbox-resolve-entry",
    keys: ["r"],
    description: "Resolve the focused Inbox entry (restore, or accept/reject a drift)",
    category: "Inbox",
    context: "outside-editable",
    contextLabel: "Inbox",
  },
] as const;

function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false;
  return Boolean(
    target.closest(
      "input, textarea, select, [role='textbox'], [contenteditable], .monaco-editor, .xterm",
    ),
  );
}

function modifierMatches(actual: boolean, expected: boolean | undefined): boolean {
  return expected === undefined || actual === expected;
}

function bindingMatches(event: KeyboardEvent, binding: ShortcutBinding): boolean {
  const key = event.key.toLowerCase();
  const expectedKeys = typeof binding.key === "string" ? [binding.key] : binding.key;
  if (!expectedKeys.some((expected) => expected.toLowerCase() === key)) return false;
  if (binding.ctrlOrMeta !== undefined) {
    if ((event.ctrlKey || event.metaKey) !== binding.ctrlOrMeta) return false;
  } else if (
    !modifierMatches(event.ctrlKey, binding.ctrl) ||
    !modifierMatches(event.metaKey, binding.meta)
  ) {
    return false;
  }
  return (
    modifierMatches(event.altKey, binding.alt) &&
    modifierMatches(event.shiftKey, binding.shift)
  );
}

export function matchAppShortcut(event: KeyboardEvent): KeyboardShortcut | undefined {
  return KEYBOARD_SHORTCUTS.find((shortcut) => {
    if (!shortcut.binding || !bindingMatches(event, shortcut.binding)) return false;
    return shortcut.context !== "outside-editable" || !isEditableTarget(event.target);
  });
}
