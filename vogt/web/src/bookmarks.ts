// Session bookmarks/favorites stored in localStorage.
import { createSignal } from "solid-js";
import { BROWSER_STORAGE_KEYS, getStoragePrefs } from "./storagePrefs";

const BOOKMARKS_KEY = BROWSER_STORAGE_KEYS.sessionBookmarks;

function load(): string[] {
  try {
    const raw = localStorage.getItem(BOOKMARKS_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter((x) => typeof x === "string") : [];
  } catch {
    return [];
  }
}

const [bookmarks, setBookmarks] = createSignal<string[]>(load());

export { bookmarks };

function persist(ids: string[]) {
  const limit = getStoragePrefs().maxSessionBookmarks;
  const next = limit <= 0 ? [] : ids.slice(0, limit);
  setBookmarks(next);
  try {
    localStorage.setItem(BOOKMARKS_KEY, JSON.stringify(next));
  } catch {
    /* non-fatal */
  }
}

export function isBookmarked(sessionId: string): boolean {
  return bookmarks().includes(sessionId);
}

export function toggleBookmark(sessionId: string) {
  const current = bookmarks();
  const next = current.includes(sessionId)
    ? current.filter((id) => id !== sessionId)
    : [sessionId, ...current];
  persist(next);
}

export function removeBookmark(sessionId: string) {
  const next = bookmarks().filter((id) => id !== sessionId);
  persist(next);
}

export function clearBookmarks() {
  persist([]);
}

export function trimBookmarks() {
  persist(bookmarks());
}
