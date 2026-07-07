const STORAGE_KEY = "mydevenv2.historyPins.v1";

function readPins(): string[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed)
      ? parsed.filter((value): value is string => typeof value === "string")
      : [];
  } catch {
    return [];
  }
}

function writePins(ids: string[]) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(ids));
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
