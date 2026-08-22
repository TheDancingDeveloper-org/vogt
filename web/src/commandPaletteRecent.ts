/** The last few commands you actually ran, kept across reloads.
 *
 * On an empty query the palette shows these first, so the thing you reach for
 * twice a day is one keystroke away instead of a scroll. Stored under a
 * `vogt.`-prefixed key (the product's current identity); a legacy
 * `mydevenv2.`-prefixed value is read once as a fallback so an upgrade does not
 * forget your history. Only stable command ids are kept — index-addressed rows
 * (a file or history match numbered by position) would resolve to a different
 * command next time, so they are never recorded.
 */

const STORAGE_KEY = "vogt.commandPalette.recent.v1";
const LEGACY_STORAGE_KEY = "mydevenv2.commandPalette.recent.v1";
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
    const current = localStorage.getItem(STORAGE_KEY);
    if (current !== null) return parse(current).slice(0, RECENT_LIMIT);
    // First run after the rename: adopt the legacy list if one is there.
    const legacy = parse(localStorage.getItem(LEGACY_STORAGE_KEY)).slice(0, RECENT_LIMIT);
    if (legacy.length > 0) {
      try {
        localStorage.setItem(STORAGE_KEY, JSON.stringify(legacy));
      } catch {
        /* non-fatal: still return what we read */
      }
    }
    return legacy;
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
    localStorage.removeItem(LEGACY_STORAGE_KEY);
  } catch {
    /* nothing to clear */
  }
}
