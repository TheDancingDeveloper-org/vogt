// Session bookmarks/favorites stored in localStorage.
import { createSignal } from "solid-js";

const BOOKMARKS_KEY = "mydevenv2.sessionBookmarks.v1";

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
  try {
    localStorage.setItem(BOOKMARKS_KEY, JSON.stringify(ids));
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
    : [...current, sessionId];
  setBookmarks(next);
  persist(next);
}

export function removeBookmark(sessionId: string) {
  const next = bookmarks().filter((id) => id !== sessionId);
  setBookmarks(next);
  persist(next);
}
