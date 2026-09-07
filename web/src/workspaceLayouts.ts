import type { LayoutMode } from "./layout";
import type { Tab, TabsStateSnapshot } from "./tabs";
import { BROWSER_STORAGE_KEYS, getStoragePrefs } from "./storagePrefs";

export interface SavedWorkspaceLayout {
  id: string;
  name: string;
  layout_mode: LayoutMode;
  tabs: Tab[];
  active: string | null;
  created_at: string;
  updated_at: string;
}

interface SaveWorkspaceLayoutInput extends TabsStateSnapshot {
  id?: string;
  name: string;
  layout_mode: LayoutMode;
}

const STORAGE_KEY = BROWSER_STORAGE_KEYS.workspaceLayouts;

function cloneTab(tab: Tab): Tab {
  return tab.kind === "editor" ? { ...tab, dirty: false } : { ...tab };
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
        dirty: false,
      };
    case "git":
      if (typeof raw.repo !== "string" || typeof raw.label !== "string") return null;
      return {
        id: raw.id,
        kind: "git",
        repo: raw.repo,
        label: raw.label,
      };
    case "gui":
    case "history":
    case "tasks":
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

function normalizeLayout(value: unknown): SavedWorkspaceLayout | null {
  if (!value || typeof value !== "object") return null;
  const raw = value as Record<string, unknown>;
  if (
    typeof raw.id !== "string" ||
    typeof raw.name !== "string" ||
    (raw.layout_mode !== "tabbed" && raw.layout_mode !== "ide") ||
    !Array.isArray(raw.tabs) ||
    typeof raw.created_at !== "string" ||
    typeof raw.updated_at !== "string"
  ) {
    return null;
  }

  const tabs = raw.tabs
    .map((tab) => normalizeTab(tab))
    .filter((tab): tab is Tab => Boolean(tab))
    .map((tab) => cloneTab(tab));
  const active = typeof raw.active === "string" ? raw.active : null;

  return {
    id: raw.id,
    name: raw.name.trim() || "Workspace layout",
    layout_mode: raw.layout_mode,
    tabs,
    active: active && tabs.some((tab) => tab.id === active) ? active : tabs[0]?.id ?? null,
    created_at: raw.created_at,
    updated_at: raw.updated_at,
  };
}

function readLayouts(): SavedWorkspaceLayout[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    const next = parsed
      .map((entry) => normalizeLayout(entry))
      .filter((entry): entry is SavedWorkspaceLayout => Boolean(entry))
      .sort((a, b) => Date.parse(b.updated_at) - Date.parse(a.updated_at));
    const limit = getStoragePrefs().maxWorkspaceLayouts;
    const trimmed = limit <= 0 ? [] : next.slice(0, limit);
    if (JSON.stringify(trimmed) !== JSON.stringify(parsed)) {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(trimmed));
    }
    return trimmed;
  } catch {
    return [];
  }
}

function writeLayouts(layouts: SavedWorkspaceLayout[]) {
  try {
    const limit = getStoragePrefs().maxWorkspaceLayouts;
    const next =
      limit <= 0
        ? []
        : [...layouts]
            .sort((a, b) => Date.parse(b.updated_at) - Date.parse(a.updated_at))
            .slice(0, limit);
    localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch {
    /* quota / private mode */
  }
}

function newLayoutId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return `layout-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

export function listWorkspaceLayouts(): SavedWorkspaceLayout[] {
  return readLayouts();
}

export function getWorkspaceLayout(id: string): SavedWorkspaceLayout | null {
  return readLayouts().find((layout) => layout.id === id) ?? null;
}

export function saveWorkspaceLayout(input: SaveWorkspaceLayoutInput): SavedWorkspaceLayout {
  const layouts = readLayouts();
  const now = new Date().toISOString();
  const existing = input.id ? layouts.find((layout) => layout.id === input.id) ?? null : null;
  const layout: SavedWorkspaceLayout = {
    id: existing?.id ?? newLayoutId(),
    name: input.name.trim() || "Workspace layout",
    layout_mode: input.layout_mode,
    tabs: input.tabs.map((tab) => cloneTab(tab)),
    active:
      input.active && input.tabs.some((tab) => tab.id === input.active)
        ? input.active
        : input.tabs[0]?.id ?? null,
    created_at: existing?.created_at ?? now,
    updated_at: now,
  };

  const next = existing
    ? layouts.map((entry) => (entry.id === layout.id ? layout : entry))
    : [layout, ...layouts];
  writeLayouts(next);
  return layout;
}

export function deleteWorkspaceLayout(id: string) {
  writeLayouts(readLayouts().filter((layout) => layout.id !== id));
}

export function clearWorkspaceLayouts() {
  writeLayouts([]);
}

export function trimWorkspaceLayouts() {
  void readLayouts();
}

export function workspaceLayoutSummary(layout: SavedWorkspaceLayout): string {
  const counts = {
    terminal: 0,
    editor: 0,
    git: 0,
    other: 0,
  };
  for (const tab of layout.tabs) {
    if (tab.kind === "terminal") counts.terminal += 1;
    else if (tab.kind === "editor") counts.editor += 1;
    else if (tab.kind === "git") counts.git += 1;
    else counts.other += 1;
  }

  const parts: string[] = [];
  if (counts.terminal) parts.push(`${counts.terminal} terminal`);
  if (counts.editor) parts.push(`${counts.editor} file`);
  if (counts.git) parts.push(`${counts.git} git`);
  if (counts.other) parts.push(`${counts.other} tool`);
  if (parts.length === 0) parts.push("empty");
  return `${layout.layout_mode} · ${parts.join(" · ")}`;
}
