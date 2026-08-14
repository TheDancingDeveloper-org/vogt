import { createStore, produce } from "solid-js/store";

export type Tab =
  | { id: string; kind: "terminal"; sessionId: string; label: string }
  | { id: string; kind: "editor"; path: string; label: string; dirty?: boolean }
  | { id: string; kind: "git"; repo: string; label: string }
  | { id: string; kind: "gui"; label: string }
  | { id: string; kind: "history"; label: string }
  | { id: string; kind: "tasks"; label: string }
  | { id: string; kind: "assistant"; label: string }
  // The Vogt surfaces (M11). A board and a backlog are one per client; a work
  // item is one tab per item, because two of them open at once is the
  // ordinary case when one blocks the other.
  | { id: string; kind: "board"; label: string }
  | { id: string; kind: "backlog"; label: string }
  | { id: string; kind: "workitem"; ref: string; label: string };

export interface TabsStateSnapshot {
  tabs: Tab[];
  /** Tab id (not session/path) currently focused, or null if none. */
  active: string | null;
}

const STORAGE_KEY = "mydevenv2.tabs.v1";

function cloneTab(tab: Tab): Tab {
  return tab.kind === "editor" ? { ...tab, dirty: Boolean(tab.dirty) } : { ...tab };
}

function normalizeTab(value: unknown): Tab | null {
  if (!value || typeof value !== "object") return null;
  const raw = value as Record<string, unknown>;
  if (typeof raw.id !== "string" || typeof raw.kind !== "string") return null;

  switch (raw.kind) {
    case "terminal":
      if (typeof raw.sessionId !== "string" || typeof raw.label !== "string") return null;
      return {
        id: raw.id,
        kind: "terminal",
        sessionId: raw.sessionId,
        label: raw.label,
      };
    case "editor":
      if (typeof raw.path !== "string" || typeof raw.label !== "string") return null;
      return {
        id: raw.id,
        kind: "editor",
        path: raw.path,
        label: raw.label,
        dirty: Boolean(raw.dirty),
      };
    case "git":
      if (typeof raw.repo !== "string" || typeof raw.label !== "string") return null;
      return {
        id: raw.id,
        kind: "git",
        repo: raw.repo,
        label: raw.label,
      };
    case "workitem":
      if (typeof raw.ref !== "string" || typeof raw.label !== "string") return null;
      return {
        id: raw.id,
        kind: "workitem",
        ref: raw.ref,
        label: raw.label,
      };
    case "gui":
    case "history":
    case "tasks":
    case "assistant":
    case "board":
    case "backlog":
      if (typeof raw.label !== "string") return null;
      return {
        id: raw.id,
        kind: raw.kind,
        label: raw.label,
      };
    default:
      return null;
  }
}

function normalizeState(value: unknown): TabsStateSnapshot {
  if (!value || typeof value !== "object") return { tabs: [], active: null };
  const raw = value as Record<string, unknown>;
  const tabs = Array.isArray(raw.tabs)
    ? raw.tabs.map((tab) => normalizeTab(tab)).filter((tab): tab is Tab => Boolean(tab))
    : [];
  const active = typeof raw.active === "string" ? raw.active : null;
  return {
    tabs: tabs.map((tab) =>
      tab.kind === "editor" ? { ...tab, dirty: false } : tab,
    ),
    active: active && tabs.some((tab) => tab.id === active) ? active : tabs[0]?.id ?? null,
  };
}

function loadInitial(): TabsStateSnapshot {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    return raw ? normalizeState(JSON.parse(raw)) : { tabs: [], active: null };
  } catch {
    return { tabs: [], active: null };
  }
}

const [store, setStore] = createStore<TabsStateSnapshot>(loadInitial());

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

export function openGuiTab(): Tab {
  const id = "gui";
  const existing = store.tabs.find((t) => t.id === id);
  if (existing) {
    setStore("active", id);
    persist();
    return existing;
  }
  const tab: Tab = { id, kind: "gui", label: "GUI" };
  setStore(
    produce((s) => {
      s.tabs.push(tab);
      s.active = id;
    }),
  );
  persist();
  return tab;
}

export function openGitTab(repo: string): Tab {
  const id = `git:${repo}`;
  const existing = store.tabs.find((t) => t.id === id);
  if (existing) {
    setStore("active", id);
    persist();
    return existing;
  }
  const label = `git: ${repo.split("/").pop() || repo || "(root)"}`;
  const tab: Tab = { id, kind: "git", repo, label };
  setStore(
    produce((s) => {
      s.tabs.push(tab);
      s.active = id;
    }),
  );
  persist();
  return tab;
}

export function openHistoryTab(): Tab {
  const id = "history";
  const existing = store.tabs.find((t) => t.id === id);
  if (existing) {
    setStore("active", id);
    persist();
    return existing;
  }
  const tab: Tab = { id, kind: "history", label: "📜 History" };
  setStore(
    produce((s) => {
      s.tabs.push(tab);
      s.active = id;
    }),
  );
  persist();
  return tab;
}

export function openTasksTab(): Tab {
  const id = "tasks";
  const existing = store.tabs.find((t) => t.id === id);
  if (existing) {
    setStore("active", id);
    persist();
    return existing;
  }
  const tab: Tab = { id, kind: "tasks", label: "Tasks" };
  setStore(
    produce((s) => {
      s.tabs.push(tab);
      s.active = id;
    }),
  );
  persist();
  return tab;
}

export function openAssistantTab(): Tab {
  const id = "assistant";
  const existing = store.tabs.find((t) => t.id === id);
  if (existing) {
    setStore("active", id);
    persist();
    return existing;
  }
  const tab: Tab = { id, kind: "assistant", label: "Assistant" };
  setStore(
    produce((s) => {
      s.tabs.push(tab);
      s.active = id;
    }),
  );
  persist();
  return tab;
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

export function snapshotTabs(): TabsStateSnapshot {
  return {
    tabs: store.tabs.map((tab) => cloneTab(tab)),
    active: store.active,
  };
}

export function replaceTabs(next: TabsStateSnapshot) {
  const normalized = normalizeState(next);
  setStore(
    produce((state) => {
      state.tabs = normalized.tabs.map((tab) => cloneTab(tab));
      state.active = normalized.active;
    }),
  );
  persist();
}

/** The board: one per client, and the surface a Vogt session usually starts from. */
export function openBoardTab(): Tab {
  return openSingletonTab("board", "board", "Board");
}

/** The ranked backlog and bugs, which share a tab and a filter set. */
export function openBacklogTab(): Tab {
  return openSingletonTab("backlog", "backlog", "Backlog");
}

/** One work item, addressable so the tab survives a reload (FR-U11). */
export function openWorkItemTab(ref: string): Tab {
  const id = `workitem:${ref}`;
  const existing = store.tabs.find((t) => t.id === id);
  if (existing) {
    setStore("active", id);
    persist();
    return existing;
  }
  const tab: Tab = { id, kind: "workitem", ref, label: ref };
  setStore(
    produce((s) => {
      s.tabs.push(tab);
      s.active = id;
    }),
  );
  persist();
  return tab;
}

/** Shared by the tabs there is exactly one of. */
function openSingletonTab(
  id: string,
  kind: "board" | "backlog",
  label: string,
): Tab {
  const existing = store.tabs.find((t) => t.id === id);
  if (existing) {
    setStore("active", id);
    persist();
    return existing;
  }
  const tab: Tab = { id, kind, label } as Tab;
  setStore(
    produce((s) => {
      s.tabs.push(tab);
      s.active = id;
    }),
  );
  persist();
  return tab;
}
