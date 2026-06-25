// Recent files tracker - stores last opened files in localStorage
const RECENT_FILES_KEY = "mydevenv2.recentFiles.v1";
const MAX_RECENT = 10;

interface RecentFile {
  path: string;
  timestamp: number;
}

export function getRecentFiles(): RecentFile[] {
  try {
    const raw = localStorage.getItem(RECENT_FILES_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter((f): f is RecentFile => f && typeof f.path === "string")
      .slice(0, MAX_RECENT);
  } catch {
    return [];
  }
}

export function addRecentFile(path: string) {
  try {
    const recent = getRecentFiles();
    const filtered = recent.filter((f) => f.path !== path);
    filtered.unshift({ path, timestamp: Date.now() });
    const limited = filtered.slice(0, MAX_RECENT);
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
