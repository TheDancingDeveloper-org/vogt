// EditorWorkspace split state - manages editor panes with drag-to-resize.
import { createStore } from "solid-js/store";

export type SplitDirection = "none" | "horizontal" | "vertical";

interface EditorPane {
  id: string;
  path: string;
  size: number; // percentage 0-100
}

interface SplitState {
  direction: SplitDirection;
  panes: EditorPane[];
  activePane: string | null;
}

const SPLIT_KEY = "mydevenv2.editorSplit.v1";

function loadInitial(): SplitState {
  try {
    const raw = localStorage.getItem(SPLIT_KEY);
    if (!raw) return { direction: "none", panes: [], activePane: null };
    return JSON.parse(raw) as SplitState;
  } catch {
    return { direction: "none", panes: [], activePane: null };
  }
}

const [splitStore, setSplitStore] = createStore<SplitState>(loadInitial());

function persist() {
  try {
    localStorage.setItem(SPLIT_KEY, JSON.stringify(splitStore));
  } catch {
    /* non-fatal */
  }
}

export { splitStore };

export function setSplitDirection(direction: SplitDirection) {
  setSplitStore("direction", direction);
  if (direction === "none") {
    setSplitStore("panes", []);
    setSplitStore("activePane", null);
  }
  persist();
}

export function addPane(path: string) {
  const id = `pane-${Date.now()}`;
  const existing = splitStore.panes.length;
  const size = existing > 0 ? 100 / (existing + 1) : 100;

  // Resize existing panes
  const resized = splitStore.panes.map((p) => ({ ...p, size }));

  setSplitStore("panes", [...resized, { id, path, size }]);
  setSplitStore("activePane", id);
  persist();
  return id;
}

export function removePane(id: string) {
  const remaining = splitStore.panes.filter((p) => p.id !== id);
  if (remaining.length === 0) {
    setSplitDirection("none");
    return;
  }

  // Redistribute sizes evenly
  const size = 100 / remaining.length;
  setSplitStore("panes", remaining.map((p) => ({ ...p, size })));

  if (splitStore.activePane === id) {
    setSplitStore("activePane", remaining[0]?.id ?? null);
  }
  persist();
}

export function updatePanePath(id: string, path: string) {
  const idx = splitStore.panes.findIndex((p) => p.id === id);
  if (idx >= 0) {
    setSplitStore("panes", idx, "path", path);
    persist();
  }
}

export function resizePanes(sizes: number[]) {
  if (sizes.length !== splitStore.panes.length) return;
  setSplitStore(
    "panes",
    splitStore.panes.map((p, i) => ({ ...p, size: sizes[i]! })),
  );
  persist();
}

export function setActivePane(id: string) {
  setSplitStore("activePane", id);
  persist();
}
