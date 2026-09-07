// Recent files tracker - stores last opened files in localStorage
import { BROWSER_STORAGE_KEYS, getStoragePrefs } from "./storagePrefs";

const RECENT_FILES_KEY = BROWSER_STORAGE_KEYS.recentFiles;

interface RecentFile {
  path: string;
  timestamp: number;
}

export function getRecentFiles(): RecentFile[] {
  try {
    const raw = localStorage.getItem(RECENT_FILES_KEY);
    if (!raw) return [];
    const limit = getStoragePrefs().maxRecentFiles;
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    const next = limit <= 0
      ? []
      : parsed
          .filter((f): f is RecentFile => f && typeof f.path === "string")
          .slice(0, limit);
    if (JSON.stringify(next) !== JSON.stringify(parsed)) {
      localStorage.setItem(RECENT_FILES_KEY, JSON.stringify(next));
    }
    return next;
  } catch {
    return [];
  }
}

export function addRecentFile(path: string) {
  try {
    const limit = getStoragePrefs().maxRecentFiles;
    if (limit <= 0) {
      clearRecentFiles();
      return;
    }
    const recent = getRecentFiles();
    const filtered = recent.filter((f) => f.path !== path);
    filtered.unshift({ path, timestamp: Date.now() });
    const limited = filtered.slice(0, limit);
    localStorage.setItem(RECENT_FILES_KEY, JSON.stringify(limited));
  } catch {
    /* quota / private mode — non-fatal */
  }
}

export function clearRecentFiles() {
  try {
    localStorage.removeItem(RECENT_FILES_KEY);
  } catch {
    /* non-fatal */
  }
}

export function trimRecentFiles() {
  void getRecentFiles();
}
