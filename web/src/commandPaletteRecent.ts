/** The last few commands you actually ran, kept across reloads.
 *
 * On an empty query the palette shows these first, so the thing you reach for
 * twice a day is one keystroke away instead of a scroll. Only stable command
 * ids are kept — index-addressed rows (a file or history match numbered by
 * position) would resolve to a different command next time, so they are never
 * recorded.
 */

const STORAGE_KEY = "vogt.commandPalette.recent.v1";
const RECENT_LIMIT = 8;

// Rows whose id encodes a list position rather than a durable identity. Their
// number means nothing on the next open, so recording them would mis-resolve.
const VOLATILE_ID_PREFIXES = ["file-", "history-", "symbol-", "recent-", "provider-"];

function isRecordable(id: string): boolean {
  return Boolean(id) && !VOLATILE_ID_PREFIXES.some((prefix) => id.startsWith(prefix));
}

function parse(raw: string | null): string[] {
  if (!raw) return [];
  try {
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed)
      ? parsed.filter((value): value is string => typeof value === "string")
      : [];
  } catch {
    return [];
  }
}

export function readRecentCommandIds(): string[] {
  try {
    return parse(localStorage.getItem(STORAGE_KEY)).slice(0, RECENT_LIMIT);
  } catch {
    return [];
  }
}

export function recordRecentCommand(id: string): void {
  if (!isRecordable(id)) return;
  const next = [id, ...readRecentCommandIds().filter((existing) => existing !== id)].slice(
    0,
    RECENT_LIMIT,
  );
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch {
    /* localStorage unavailable (private mode, disabled) — recency is best-effort */
  }
}

export function clearRecentCommands(): void {
  try {
    localStorage.removeItem(STORAGE_KEY);
  } catch {
    /* nothing to clear */
  }
}
