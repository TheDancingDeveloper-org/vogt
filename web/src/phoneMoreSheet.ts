/**
 * The phone bottom bar shows four primary places (Sessions, Inbox, Board,
 * Backlog). Every other place — and the two account actions that used to need
 * a command-palette round trip — lives behind the fifth "More" slot, which
 * opens a bottom sheet. This is the single source the sheet renders and the
 * unit test asserts against, so the gating that decides which rows appear is
 * testable without mounting the shell.
 */
export interface MoreSheetInput {
  /** Whether a Vogt core is configured — gates Projects and Audit, exactly as
   *  the desktop rail's Estate group does. */
  vogtConfigured: boolean;
  /** `publicCfg()?.gui_stream_available` — gates the GUI stream row. */
  guiEnabled: boolean;
  /** `publicCfg()?.assistant_enabled` — gates the Assistant row. */
  assistantEnabled: boolean;
}

export type MoreSheetItem =
  | { kind: "place"; id: string; label: string; href: string }
  | { kind: "action"; id: "settings" | "signout"; label: string };

/**
 * The places that the four-slot bottom bar cannot reach, plus Settings and
 * Sign out. Order mirrors the desktop rail: Estate (Projects, Audit) then
 * Machine (Git, History, Tasks, GUI stream, Assistant), then the account
 * actions last.
 */
export function moreSheetItems(input: MoreSheetInput): MoreSheetItem[] {
  const items: MoreSheetItem[] = [];
  if (input.vogtConfigured) {
    items.push({ kind: "place", id: "projects", label: "Projects", href: "#/projects" });
    items.push({ kind: "place", id: "audit", label: "Audit", href: "#/audit" });
  }
  items.push({ kind: "place", id: "git", label: "Git", href: "#/g" });
  items.push({ kind: "place", id: "history", label: "History", href: "#/history" });
  items.push({ kind: "place", id: "tasks", label: "Tasks", href: "#/tasks" });
  if (input.guiEnabled) {
    items.push({ kind: "place", id: "gui", label: "GUI stream", href: "#/gui" });
  }
  if (input.assistantEnabled) {
    items.push({ kind: "place", id: "assistant", label: "Assistant", href: "#/assistant" });
  }
  items.push({ kind: "action", id: "settings", label: "Settings" });
  items.push({ kind: "action", id: "signout", label: "Sign out" });
  return items;
}

/** The place rows only, for callers that render links separately from actions. */
export function moreSheetPlaces(
  input: MoreSheetInput,
): Extract<MoreSheetItem, { kind: "place" }>[] {
  return moreSheetItems(input).filter(
    (item): item is Extract<MoreSheetItem, { kind: "place" }> => item.kind === "place",
  );
}
