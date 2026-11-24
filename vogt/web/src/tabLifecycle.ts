import type { Tab } from "./tabs";

export type TabRetention = "always" | "active";

/** The resource policy for one open Sessions pane. */
export function tabRetention(tab: Tab): TabRetention {
  if (tab.kind === "terminal") return "always";
  if (tab.kind === "tasks" && tab.dirty) return "always";
  return "active";
}

export function shouldMountTab(
  tab: Tab,
  activeTabId: string | null,
  narrow = false,
): boolean {
  if (narrow && tab.kind === "terminal") return tab.id === activeTabId;
  const retention = tabRetention(tab);
  if (retention === "always") return true;
  return tab.id === activeTabId;
}

/**
 * Live terminal panes kept mounted at once (WI-3129). Each mounted xterm holds
 * a WebGL context and keeps parsing output, and Chrome caps a page at about
 * 16 contexts. The rest unmount: their cache is saved on the way out, and a
 * revisit restores it. Matches `MAX_DORMANT_SOCKETS`.
 */
export const MAX_LIVE_TERMINALS = 4;

/**
 * The live terminal set after `activeId` is shown: the shown terminal first,
 * then the most recently shown ones, capped. Ids of terminals no longer open
 * drop out. A non-terminal `activeId` leaves the set as it was.
 */
export function touchRecentTerminal(
  recent: readonly string[],
  activeId: string | null,
  tabs: readonly Tab[],
  limit = MAX_LIVE_TERMINALS,
): string[] {
  const open = new Set(tabs.filter((tab) => tab.kind === "terminal").map((tab) => tab.id));
  const kept = recent.filter((id) => open.has(id) && id !== activeId);
  if (activeId !== null && open.has(activeId)) kept.unshift(activeId);
  return kept.slice(0, limit);
}

/** Retain terminal/xterm state while parking the socket for inactive panes. */
export function terminalPaneParked(
  workspaceActive: boolean,
  paneActive: boolean,
): boolean {
  return !workspaceActive || !paneActive;
}

export function hasUnsavedWork(tabs: readonly Tab[]): boolean {
  return tabs.some((tab) =>
    (tab.kind === "editor" || tab.kind === "tasks") && Boolean(tab.dirty),
  );
}

/** Kept for callers and tests that only need the historical editor predicate. */
export function hasDirtyEditor(tabs: readonly Tab[]): boolean {
  return tabs.some((tab) => tab.kind === "editor" && Boolean(tab.dirty));
}

export function protectDirtyEditorExit(event: BeforeUnloadEvent): void {
  event.preventDefault();
  // Chromium still checks returnValue; assigning the empty string avoids
  // deprecated custom copy while requesting the browser-owned confirmation.
  event.returnValue = "";
}
