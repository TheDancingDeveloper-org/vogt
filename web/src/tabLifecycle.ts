import type { Tab } from "./tabs";

export type TabRetention = "always" | "active";

/** The resource policy for one open Sessions pane. */
export function tabRetention(tab: Tab): TabRetention {
  if (tab.kind === "terminal") return "always";
  if (tab.kind === "tasks" && tab.dirty) return "always";
  return "active";
}

export function shouldMountTab(tab: Tab, activeTabId: string | null): boolean {
  const retention = tabRetention(tab);
  if (retention === "always") return true;
  return tab.id === activeTabId;
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
