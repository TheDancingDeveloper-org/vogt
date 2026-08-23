export interface StoragePrefs {
  maxRecentFiles: number;
  maxWorkspaceLayouts: number;
  maxAuthProfiles: number;
  maxSessionBookmarks: number;
  maxHistoryPins: number;
  defaultSessionScrollbackBytes: number;
}

export const BROWSER_STORAGE_KEYS = {
  recentFiles: "vogt.recentFiles.v1",
  workspaceLayouts: "vogt.workspaceLayouts.v1",
  authProfiles: "vogt.authProfiles.v1",
  sessionBookmarks: "vogt.sessionBookmarks.v1",
  historyPins: "vogt.historyPins.v1",
  prefs: "vogt.storagePrefs.v1",
} as const;

export const DEFAULT_STORAGE_PREFS: StoragePrefs = {
  maxRecentFiles: 10,
  maxWorkspaceLayouts: 12,
  maxAuthProfiles: 6,
  maxSessionBookmarks: 40,
  maxHistoryPins: 40,
  defaultSessionScrollbackBytes: 0,
};

function clampInteger(
  value: unknown,
  fallback: number,
  {
    min = 0,
    max = Number.MAX_SAFE_INTEGER,
  }: { min?: number; max?: number } = {},
): number {
  const parsed =
    typeof value === "number"
      ? value
      : typeof value === "string" && value.trim()
        ? Number(value)
        : Number.NaN;
  if (!Number.isFinite(parsed)) return fallback;
  return Math.min(max, Math.max(min, Math.round(parsed)));
}

function normalizeStoragePrefs(value: unknown): StoragePrefs {
  const raw = value && typeof value === "object" ? (value as Record<string, unknown>) : {};
  return {
    maxRecentFiles: clampInteger(raw.maxRecentFiles, DEFAULT_STORAGE_PREFS.maxRecentFiles, {
      max: 200,
    }),
    maxWorkspaceLayouts: clampInteger(
      raw.maxWorkspaceLayouts,
      DEFAULT_STORAGE_PREFS.maxWorkspaceLayouts,
      { max: 100 },
    ),
    maxAuthProfiles: clampInteger(raw.maxAuthProfiles, DEFAULT_STORAGE_PREFS.maxAuthProfiles, {
      max: 50,
    }),
    maxSessionBookmarks: clampInteger(
      raw.maxSessionBookmarks,
      DEFAULT_STORAGE_PREFS.maxSessionBookmarks,
      { max: 200 },
    ),
    maxHistoryPins: clampInteger(raw.maxHistoryPins, DEFAULT_STORAGE_PREFS.maxHistoryPins, {
      max: 200,
    }),
    defaultSessionScrollbackBytes: clampInteger(
      raw.defaultSessionScrollbackBytes,
      DEFAULT_STORAGE_PREFS.defaultSessionScrollbackBytes,
      { max: 16 * 1024 * 1024 },
    ),
  };
}

export function getStoragePrefs(): StoragePrefs {
  try {
    const raw = localStorage.getItem(BROWSER_STORAGE_KEYS.prefs);
    return raw ? normalizeStoragePrefs(JSON.parse(raw)) : DEFAULT_STORAGE_PREFS;
  } catch {
    return DEFAULT_STORAGE_PREFS;
  }
}

export function saveStoragePrefs(prefs: Partial<StoragePrefs> | StoragePrefs): StoragePrefs {
  const merged = normalizeStoragePrefs({
    ...getStoragePrefs(),
    ...prefs,
  });
  try {
    localStorage.setItem(BROWSER_STORAGE_KEYS.prefs, JSON.stringify(merged));
  } catch {
    /* localStorage unavailable */
  }
  return merged;
}

export function formatScrollbackBytes(bytes: number): string {
  if (!bytes) return "Server default";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KiB`;
  return `${(bytes / (1024 * 1024)).toFixed(bytes % (1024 * 1024) === 0 ? 0 : 1)} MiB`;
}
