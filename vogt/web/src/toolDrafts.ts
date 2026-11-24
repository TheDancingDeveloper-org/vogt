/** Per-window state for Sessions tools that intentionally unmount.
 *
 * The in-memory Map keeps a draft alive while a tool's tab is closed and
 * reopened inside the same window. That is enough for tab churn, but not for
 * a full reload: the heap resets and the draft is gone. So every write is
 * mirrored to `sessionStorage`, and a read that misses the Map falls back to
 * it — a reload rehydrates from storage into a fresh Map. sessionStorage
 * (not localStorage) because a draft belongs to this tab's session, not to
 * every tab the browser will ever open on this origin.
 */
const STORAGE_PREFIX = "vogt.toolDraft.";
const toolDrafts = new Map<string, unknown>();

function storageKey(key: string): string {
  return `${STORAGE_PREFIX}${key}`;
}

export function readToolDraft<T>(key: string, fallback: T): T {
  if (toolDrafts.has(key)) return toolDrafts.get(key) as T;
  try {
    const raw = sessionStorage.getItem(storageKey(key));
    if (raw !== null) {
      const value = JSON.parse(raw) as T;
      toolDrafts.set(key, value);
      return value;
    }
  } catch {
    // sessionStorage unavailable (private mode, disabled) or bad JSON — the
    // Map is still authoritative for this window, so fall through.
  }
  return fallback;
}

export function writeToolDraft<T>(key: string, value: T): void {
  toolDrafts.set(key, value);
  try {
    sessionStorage.setItem(storageKey(key), JSON.stringify(value));
  } catch {
    // Non-fatal: the in-memory Map still carries the draft for this window.
  }
}

export function clearToolDrafts(): void {
  toolDrafts.clear();
  try {
    for (let i = sessionStorage.length - 1; i >= 0; i -= 1) {
      const key = sessionStorage.key(i);
      if (key && key.startsWith(STORAGE_PREFIX)) sessionStorage.removeItem(key);
    }
  } catch {
    // sessionStorage unavailable — the Map is already cleared, nothing to do.
  }
}
