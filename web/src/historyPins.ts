import { BROWSER_STORAGE_KEYS, getStoragePrefs } from "./storagePrefs";

const STORAGE_KEY = BROWSER_STORAGE_KEYS.historyPins;

function readPins(): string[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    const next = Array.isArray(parsed)
      ? parsed.filter((value): value is string => typeof value === "string")
      : [];
    const limit = getStoragePrefs().maxHistoryPins;
    const trimmed = limit <= 0 ? [] : next.slice(0, limit);
    if (JSON.stringify(trimmed) !== JSON.stringify(parsed)) {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(trimmed));
    }
    return trimmed;
  } catch {
    return [];
  }
}

function writePins(ids: string[]) {
  const limit = getStoragePrefs().maxHistoryPins;
  const next = limit <= 0 ? [] : ids.slice(0, limit);
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch {
    /* localStorage unavailable */
  }
}

export function getPinnedHistoryIds(): string[] {
  return readPins();
}

export function isHistoryPinned(id: string): boolean {
  return readPins().includes(id);
}

export function toggleHistoryPin(id: string): string[] {
  const current = readPins();
  const next = current.includes(id)
    ? current.filter((entry) => entry !== id)
    : [id, ...current];
  writePins(next);
  return next;
}

export function removeHistoryPin(id: string): string[] {
  const next = readPins().filter((entry) => entry !== id);
  writePins(next);
  return next;
}

export function clearHistoryPins(): string[] {
  writePins([]);
  return [];
}

export function trimHistoryPins(): string[] {
  return readPins();
}
