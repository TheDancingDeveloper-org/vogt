import { createStore } from "solid-js/store";
import type { Tab } from "./tabs";

export type SplitDirection = "none" | "horizontal" | "vertical";

export interface EditorPane {
  tabId: string;
  size: number;
}

interface SplitState {
  direction: SplitDirection;
  panes: EditorPane[];
  activePane: string | null;
}

const SPLIT_KEY = "mydevenv2.editorSplit.v1";
const MIN_PANE_SIZE = 10;

function emptyState(): SplitState {
  return { direction: "none", panes: [], activePane: null };
}

function loadInitial(): SplitState {
  try {
    const raw = localStorage.getItem(SPLIT_KEY);
    if (!raw) return emptyState();
    const parsed = JSON.parse(raw) as SplitState;
    if (
      parsed.direction !== "horizontal" &&
      parsed.direction !== "vertical" &&
      parsed.direction !== "none"
    ) {
      return emptyState();
    }
    return {
      direction: parsed.direction,
      panes: Array.isArray(parsed.panes) ? parsed.panes : [],
      activePane: parsed.activePane ?? null,
    };
  } catch {
    return emptyState();
  }
}

const [splitStore, setSplitStore] = createStore<SplitState>(loadInitial());

function persist() {
  try {
    localStorage.setItem(SPLIT_KEY, JSON.stringify(splitStore));
  } catch {
    /* quota / private mode: non-fatal */
  }
}

function evenPanes(tabIds: string[]): EditorPane[] {
  const unique = [...new Set(tabIds)];
  const size = unique.length > 0 ? 100 / unique.length : 100;
  return unique.map((tabId) => ({ tabId, size }));
}

function normalizePanes(panes: EditorPane[]): EditorPane[] {
  if (panes.length === 0) return [];
  const total = panes.reduce((sum, pane) => sum + pane.size, 0);
  if (total <= 0) return evenPanes(panes.map((pane) => pane.tabId));
  return panes.map((pane) => ({
    ...pane,
    size: (pane.size / total) * 100,
  }));
}

function editorTabIds(editorTabs: Extract<Tab, { kind: "editor" }>[]): string[] {
  return editorTabs.map((tab) => tab.id);
}

export { splitStore };

export function setSplitDirection(
  direction: SplitDirection,
  editorTabs: Extract<Tab, { kind: "editor" }>[],
  activeTabId: string | null,
) {
  if (direction === "none") {
    setSplitStore(emptyState());
    persist();
    return;
  }

  const ids = editorTabIds(editorTabs);
  if (ids.length < 2 || !activeTabId || !ids.includes(activeTabId)) {
    setSplitStore(emptyState());
    persist();
    return;
  }

  const existing = splitStore.panes
    .map((pane) => pane.tabId)
    .filter((id) => ids.includes(id));
  const panes = existing.includes(activeTabId)
    ? existing
    : [activeTabId, ...existing];
  const nextPaneIds =
    panes.length >= 2
      ? panes
      : [activeTabId, ids.find((id) => id !== activeTabId)!];

  setSplitStore("direction", direction);
  setSplitStore("panes", evenPanes(nextPaneIds));
  setSplitStore("activePane", activeTabId);
  persist();
}

export function showTabInActivePane(tabId: string) {
  if (splitStore.direction === "none") return;
  const existingIndex = splitStore.panes.findIndex((pane) => pane.tabId === tabId);
  if (existingIndex >= 0) {
    setActivePane(tabId);
    return;
  }

  const targetIndex = Math.max(
    0,
    splitStore.panes.findIndex((pane) => pane.tabId === splitStore.activePane),
  );
  if (splitStore.panes.length === 0) return;
  setSplitStore("panes", targetIndex, "tabId", tabId);
  setSplitStore("activePane", tabId);
  persist();
}

export function removePane(tabId: string) {
  const remaining = splitStore.panes.filter((pane) => pane.tabId !== tabId);
  if (remaining.length < 2) {
    setSplitStore(emptyState());
    persist();
    return;
  }

  setSplitStore("panes", evenPanes(remaining.map((pane) => pane.tabId)));
  if (splitStore.activePane === tabId) {
    setSplitStore("activePane", remaining[0]?.tabId ?? null);
  }
  persist();
}

export function resizePanePair(
  index: number,
  deltaPercent: number,
  basePanes: EditorPane[] = splitStore.panes.map((pane) => ({ ...pane })),
) {
  const panes = basePanes;
  if (index < 0 || index >= panes.length - 1) return;

  const left = panes[index]!;
  const right = panes[index + 1]!;
  const pairTotal = left.size + right.size;
  const leftSize = Math.max(
    MIN_PANE_SIZE,
    Math.min(pairTotal - MIN_PANE_SIZE, left.size + deltaPercent),
  );
  const rightSize = pairTotal - leftSize;
  const next = splitStore.panes.map((pane, paneIndex) => {
    if (paneIndex === index) return { ...pane, size: leftSize };
    if (paneIndex === index + 1) return { ...pane, size: rightSize };
    return pane;
  });

  setSplitStore("panes", normalizePanes(next));
  persist();
}

export function setActivePane(tabId: string) {
  setSplitStore("activePane", tabId);
  persist();
}

export function syncSplitPanes(
  editorTabs: Extract<Tab, { kind: "editor" }>[],
  activeTabId: string | null,
) {
  if (splitStore.direction === "none") return;

  const validIds = new Set(editorTabIds(editorTabs));
  const panes = splitStore.panes.filter((pane) => validIds.has(pane.tabId));
  if (panes.length < 2) {
    setSplitStore(emptyState());
    persist();
    return;
  }

  const activePane =
    activeTabId && panes.some((pane) => pane.tabId === activeTabId)
      ? activeTabId
      : panes.some((pane) => pane.tabId === splitStore.activePane)
        ? splitStore.activePane
        : panes[0]!.tabId;

  setSplitStore("panes", normalizePanes(panes));
  setSplitStore("activePane", activePane);
  persist();
}
