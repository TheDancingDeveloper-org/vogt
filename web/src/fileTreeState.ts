// File-tree view state, hoisted out of the components so it survives a tab
// switch and a workspace remount (#238). Before this, folder expansion and the
// search box lived in component-local signals: switching to another tab
// unmounted the tree, and switching back reset every expanded folder and lost
// the query. Keyed by path, like `railSections.ts`, so the state is addressable
// and one folder's toggle can't clobber another's.
import { createSignal } from "solid-js";
import { createStore } from "solid-js/store";

const EXPANDED_KEY = "mydevenv2.fileTree.expanded.v1";
const SIDEBAR_KEY = "mydevenv2.fileTree.sidebarCollapsed.v1";

function readExpanded(): Record<string, boolean> {
  try {
    const raw = localStorage.getItem(EXPANDED_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    const out: Record<string, boolean> = {};
    for (const [path, open] of Object.entries(parsed)) {
      if (open === true) out[path] = true;
    }
    return out;
  } catch {
    return {};
  }
}

function readSidebarCollapsed(): boolean {
  try {
    return localStorage.getItem(SIDEBAR_KEY) === "1";
  } catch {
    return false;
  }
}

const [expanded, setExpandedStore] = createStore<Record<string, boolean>>(readExpanded());

/** Whether the folder at `path` is currently expanded. Reactive. */
export function isExpanded(path: string): boolean {
  return expanded[path] === true;
}

/** Remember (or forget) that a folder is expanded, persisting across remounts. */
export function setExpanded(path: string, open: boolean): void {
  setExpandedStore(path, open);
  try {
    // Persist only the open set — a closed folder is the default, so it need
    // not carry a `false` around forever.
    const snapshot: Record<string, boolean> = {};
    for (const [key, value] of Object.entries(expanded)) {
      if (value === true) snapshot[key] = true;
    }
    localStorage.setItem(EXPANDED_KEY, JSON.stringify(snapshot));
  } catch {
    // Private mode / quota: expansion still works, just does not persist.
  }
}

const [sidebarCollapsed, setSidebarCollapsedSignal] = createSignal(readSidebarCollapsed());

/** Whether the editor workspace's Files sidebar is collapsed. Reactive. */
export { sidebarCollapsed };

export function setSidebarCollapsed(collapsed: boolean): void {
  setSidebarCollapsedSignal(collapsed);
  try {
    localStorage.setItem(SIDEBAR_KEY, collapsed ? "1" : "0");
  } catch {
    // Non-fatal — the collapse still applies for this mount.
  }
}

// The file-tree search query. Kept in memory (not persisted): it should survive
// a tab switch within a session but not resurrect a stale search on next boot.
const [searchQuery, setSearchQuerySignal] = createSignal("");

export { searchQuery as fileTreeSearch };

export function setFileTreeSearch(query: string): void {
  setSearchQuerySignal(query);
}

// Per-folder invalidation. A file op inside the tree bumps only the affected
// parent's counter, so exactly that folder refetches its children — the rest of
// the tree keeps its loaded, expanded state instead of collapsing (#238). "" is
// the root level.
const [folderVersions, setFolderVersions] = createStore<Record<string, number>>({});

/** The invalidation counter for a folder path ("" = root). Track it to refetch
 *  that folder's children when it moves. */
export function folderVersion(path: string): number {
  return folderVersions[path] ?? 0;
}

/** Mark a single folder's children stale — the parent of whatever just changed. */
export function invalidateFolder(path: string): void {
  setFolderVersions(path, (n) => (n ?? 0) + 1);
}

/** Every currently-expanded folder path — for a full manual refresh. */
export function expandedPaths(): string[] {
  return Object.keys(expanded).filter((path) => expanded[path] === true);
}

/** The parent folder of a workspace-relative path ("" when it sits at the root). */
export function parentFolder(path: string): string {
  const clean = path.replace(/\/+$/, "");
  const idx = clean.lastIndexOf("/");
  return idx >= 0 ? clean.slice(0, idx) : "";
}

/** Re-read persisted state (tests clear localStorage; the module singleton
 *  otherwise keeps its in-memory values). */
export function resetFileTreeState(): void {
  // Clear every current key before re-reading, so a stale expansion from an
  // earlier test does not survive a `localStorage.clear()`.
  for (const key of Object.keys(expanded)) setExpandedStore(key, false);
  const fresh = readExpanded();
  for (const [key, value] of Object.entries(fresh)) setExpandedStore(key, value);
  setSidebarCollapsedSignal(readSidebarCollapsed());
  setSearchQuerySignal("");
}
