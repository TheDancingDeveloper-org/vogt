import { untrack } from "solid-js";
import { createStore, produce } from "solid-js/store";
import { PLACES_MIGRATION_KEY, PLACES_STATE_KEY } from "./layout";

export type Tab =
  | { id: string; kind: "terminal"; sessionId: string; label: string }
  | { id: string; kind: "editor"; path: string; label: string; dirty?: boolean }
  | { id: string; kind: "git"; repo: string; label: string }
  | { id: string; kind: "gui"; label: string }
  | { id: string; kind: "history"; label: string }
  | { id: string; kind: "tasks"; label: string; dirty?: boolean }
  | { id: string; kind: "assistant"; label: string }
  | { id: string; kind: "workitem"; ref: string; label: string };

export interface TabsStateSnapshot {
  tabs: Tab[];
  /** Tab id (not session/path) currently focused, or null if none. */
  active: string | null;
}

export interface RecentPlace {
  path: string;
  label: string;
}

interface PlacesStateSnapshot {
  places: RecentPlace[];
}

export interface LegacyMigrationResult {
  state: TabsStateSnapshot;
  places: RecentPlace[];
  initialRoute: string | null;
}

const STORAGE_KEY = "mydevenv2.tabs.v2";
const LEGACY_STORAGE_KEY = "mydevenv2.tabs.v1";

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
    case "tasks":
      if (typeof raw.label !== "string") return null;
      return {
        id: raw.id,
        kind: "tasks",
        label: raw.label,
        dirty: Boolean(raw.dirty),
      };
    case "gui":
    case "history":
    case "assistant":
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
      tab.kind === "editor" || tab.kind === "tasks"
        ? { ...tab, dirty: false }
        : tab,
    ),
    active: active && tabs.some((tab) => tab.id === active) ? active : tabs[0]?.id ?? null,
  };
}

const SURFACE_ROUTES: Record<string, string> = {
  board: "/board",
  backlog: "/backlog",
  inbox: "/inbox",
  projects: "/projects",
  audit: "/audit",
};

function legacyRoute(raw: Record<string, unknown>): string | null {
  const kind = typeof raw.kind === "string" ? raw.kind : "";
  const surface = SURFACE_ROUTES[kind];
  if (surface) return surface;
  if (kind === "workitem" && typeof raw.ref === "string") {
    return `/w/${encodeURIComponent(raw.ref)}`;
  }
  if (kind === "terminal" && typeof raw.sessionId === "string") {
    return `/t/${encodeURIComponent(raw.sessionId)}`;
  }
  if (kind === "editor" && typeof raw.path === "string") {
    return `/e/${encodeURIComponent(raw.path)}`;
  }
  if (kind === "git" && typeof raw.repo === "string") {
    return raw.repo ? `/g/${encodeURIComponent(raw.repo)}` : "/g";
  }
  if (kind === "gui") return "/gui";
  if (kind === "history") return "/history";
  if (kind === "tasks") return "/tasks";
  if (kind === "assistant") return "/assistant";
  return null;
}

function placeLabel(raw: Record<string, unknown>, path: string): string {
  if (typeof raw.label === "string" && raw.label.trim()) return raw.label;
  if (path.startsWith("/w/")) return decodeURIComponent(path.slice(3));
  return path === "/g" ? "Git" : path.slice(1) || "Vogt";
}

function addPlace(places: RecentPlace[], path: string, label: string) {
  if (places.some((place) => place.path === path)) return;
  places.push({ path, label });
}

/**
 * Convert the pre-places tab snapshot without touching browser storage.
 * Keeping this pure makes the destructive edge of migration testable: all
 * valid neighbours survive malformed entries, and the old key can remain
 * untouched until the caller has written every replacement record.
 */
export function migrateLegacyState(value: unknown): LegacyMigrationResult {
  if (!value || typeof value !== "object") {
    return { state: { tabs: [], active: null }, places: [], initialRoute: null };
  }
  const rawState = value as Record<string, unknown>;
  const rawTabs = Array.isArray(rawState.tabs) ? rawState.tabs : [];
  const tabs: Tab[] = [];
  const places: RecentPlace[] = [];
  const activeId = typeof rawState.active === "string" ? rawState.active : null;
  let initialRoute: string | null = null;

  for (const value of rawTabs) {
    if (!value || typeof value !== "object") continue;
    const raw = value as Record<string, unknown>;
    const kind = typeof raw.kind === "string" ? raw.kind : "";
    const route = legacyRoute(raw);
    if (kind === "workitem" || SURFACE_ROUTES[kind]) {
      if (route) {
        addPlace(places, route, placeLabel(raw, route));
        if (raw.id === activeId) initialRoute = route;
      }
      continue;
    }
    const tab = normalizeTab(raw);
    if (!tab) continue;
    tabs.push(tab);
    if (raw.id === activeId) initialRoute = route;
  }

  const active = activeId && tabs.some((tab) => tab.id === activeId)
    ? activeId
    : tabs[0]?.id ?? null;
  return {
    state: { tabs, active },
    places: places.slice(0, 12),
    initialRoute,
  };
}

function readPlaces(): RecentPlace[] {
  try {
    const raw = localStorage.getItem(PLACES_STATE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as Partial<PlacesStateSnapshot>;
    if (!Array.isArray(parsed.places)) return [];
    return parsed.places.filter(
      (place): place is RecentPlace =>
        Boolean(place) && typeof place.path === "string" && typeof place.label === "string",
    ).slice(0, 12);
  } catch {
    return [];
  }
}

let migratedInitialRoute: string | null = null;
let migratedPlaces: RecentPlace[] = [];

function loadInitial(): TabsStateSnapshot {
  try {
    const current = localStorage.getItem(STORAGE_KEY);
    const legacy = localStorage.getItem(LEGACY_STORAGE_KEY);
    const marker = localStorage.getItem(PLACES_MIGRATION_KEY);
    if (legacy && marker !== "v1") {
      const migration = migrateLegacyState(JSON.parse(legacy));
      // The legacy value is intentionally not removed. A failed write leaves
      // it available for the next boot to retry, while the marker is written
      // only after both replacement values are durable.
      localStorage.setItem(STORAGE_KEY, JSON.stringify(migration.state));
      localStorage.setItem(
        PLACES_STATE_KEY,
        JSON.stringify({ places: migration.places }),
      );
      localStorage.setItem(PLACES_MIGRATION_KEY, "v1");
      migratedInitialRoute = migration.initialRoute;
      migratedPlaces = migration.places;
      return migration.state;
    }
    const normalized = current
      ? normalizeState(JSON.parse(current))
      : { tabs: [], active: null };
    localStorage.setItem(STORAGE_KEY, JSON.stringify(normalized));
    migratedPlaces = readPlaces();
    return normalized;
  } catch {
    return { tabs: [], active: null };
  }
}

const [store, setStore] = createStore<TabsStateSnapshot>(loadInitial());
const [placesStore, setPlacesStore] = createStore<PlacesStateSnapshot>({
  places: migratedPlaces.length ? migratedPlaces : readPlaces(),
});

function persist() {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(store));
  } catch {
    /* quota / private mode — non-fatal */
  }
}

export const tabsStore = store;
export const recentPlacesStore = placesStore;

export function initialRoute(): string | null {
  return migratedInitialRoute;
}

export function rememberPlace(path: string, label: string): void {
  if (!path || path === "/") return;
  const current = untrack(() => placesStore.places);
  const next = [
    { path, label },
    ...current.filter((place) => place.path !== path),
  ].slice(0, 12);
  setPlacesStore("places", next);
  try {
    localStorage.setItem(PLACES_STATE_KEY, JSON.stringify({ places: next }));
  } catch {
    /* recent navigation is presentation state */
  }
}

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
  const tab: Tab = { id, kind: "history", label: "History" };
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

export function setTasksDirty(dirty: boolean) {
  setStore(
    produce((state) => {
      const tab = state.tabs.find((candidate) => candidate.id === "tasks");
      if (tab?.kind === "tasks") tab.dirty = dirty;
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
