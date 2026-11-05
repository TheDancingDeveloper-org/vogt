import type { Tab } from "./tabs";

export type TabRetention = "always" | "active";

/** The resource policy for one open Sessions pane. */
export function tabRetention(tab: Tab): TabRetention {
  if (tab.kind === "terminal") return "always";
  if (tab.kind === "tasks" && tab.dirty) return "always";
  return "active";
}

/**
 * How many terminal tabs stay mounted at once: the active one plus the most
 * recently left. Each mounted terminal holds an xterm, a WebGL context and
 * a glyph atlas, and browsers cap WebGL contexts per page (WI-3129).
 */
export const MAX_LIVE_TERMINAL_TABS = 3;

/** Move `tabId` to the front of the recency list, bounded to `max`. */
export function touchRecentTerminal(
  recent: readonly string[],
  tabId: string,
  max = MAX_LIVE_TERMINAL_TABS,
): string[] {
  return [tabId, ...recent.filter((id) => id !== tabId)].slice(0, max);
}

export function shouldMountTab(
  tab: Tab,
  activeTabId: string | null,
  narrow = false,
  liveTerminals?: ReadonlySet<string>,
): boolean {
  if (narrow && tab.kind === "terminal") return tab.id === activeTabId;
  if (tab.kind === "terminal" && liveTerminals) {
    return tab.id === activeTabId || liveTerminals.has(tab.id);
  }
  const retention = tabRetention(tab);
  if (retention === "always") return true;
  return tab.id === activeTabId;
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
