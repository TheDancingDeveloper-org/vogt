import { createStore, produce } from "solid-js/store";

export type Tab =
  | { id: string; kind: "terminal"; sessionId: string; label: string }
  | { id: string; kind: "editor"; path: string; label: string; dirty?: boolean };

interface TabsStore {
  tabs: Tab[];
  /** Tab id (not session/path) currently focused, or null if none. */
  active: string | null;
}

const STORAGE_KEY = "mydevenv2.tabs.v1";

function loadInitial(): TabsStore {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { tabs: [], active: null };
    const parsed = JSON.parse(raw) as TabsStore;
    // Reset dirty flags — they're per-session state not worth persisting.
    parsed.tabs = parsed.tabs.map((t) =>
      t.kind === "editor" ? { ...t, dirty: false } : t,
    );
    return parsed;
  } catch {
    return { tabs: [], active: null };
  }
}

const [store, setStore] = createStore<TabsStore>(loadInitial());

function persist() {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(store));
  } catch {
    /* quota / private mode — non-fatal */
  }
}

export const tabsStore = store;

export function openTerminalTab(sessionId: string, label: string): Tab {
  const id = `term:${sessionId}`;
  const existing = store.tabs.find((t) => t.id === id);
  if (existing) {
    setStore("active", id);
    persist();
    return existing;
  }
  const tab: Tab = { id, kind: "terminal", sessionId, label };
  setStore(
    produce((s) => {
      s.tabs.push(tab);
      s.active = id;
    }),
  );
  persist();
  return tab;
}

export function openEditorTab(path: string): Tab {
  const id = `edit:${path}`;
  const existing = store.tabs.find((t) => t.id === id);
  if (existing) {
    setStore("active", id);
    persist();
    return existing;
  }
  const label = path.split("/").pop() ?? path;
  const tab: Tab = { id, kind: "editor", path, label, dirty: false };
  setStore(
    produce((s) => {
      s.tabs.push(tab);
      s.active = id;
    }),
  );
  persist();
  return tab;
}

export function closeTab(id: string) {
  setStore(
    produce((s) => {
      const idx = s.tabs.findIndex((t) => t.id === id);
      if (idx === -1) return;
      s.tabs.splice(idx, 1);
      if (s.active === id) {
        s.active = s.tabs[idx]?.id ?? s.tabs[idx - 1]?.id ?? null;
      }
    }),
  );
  persist();
}

export function focusTab(id: string) {
  if (store.tabs.some((t) => t.id === id)) {
    setStore("active", id);
    persist();
  }
}

export function focusTabBySessionId(sessionId: string) {
  const t = store.tabs.find(
    (t) => t.kind === "terminal" && t.sessionId === sessionId,
  );
  if (t) focusTab(t.id);
}

export function focusTabByPath(path: string) {
  const t = store.tabs.find(
    (t) => t.kind === "editor" && t.path === path,
  );
  if (t) focusTab(t.id);
}

export function renameTab(id: string, label: string) {
  setStore(
    produce((s) => {
      const t = s.tabs.find((t) => t.id === id);
      if (t) t.label = label;
    }),
  );
  persist();
}

export function setEditorDirty(id: string, dirty: boolean) {
  setStore(
    produce((s) => {
      const t = s.tabs.find((t) => t.id === id);
      if (t && t.kind === "editor") t.dirty = dirty;
    }),
  );
}

export function activeTab(): Tab | null {
  return store.tabs.find((t) => t.id === store.active) ?? null;
}
