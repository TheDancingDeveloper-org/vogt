// Which places-rail sections are open, shared across the rail's own Running
// and Recent places headers and FileTree's Files header — one JSON object so
// a write from either side cannot drop the other's field (rail-spec.md B3).
import { createStore } from "solid-js/store";

const KEY = "vogt.rail.sections.v1";

export interface RailSections {
  running: boolean;
  recent: boolean;
  files: boolean;
}

const DEFAULTS: RailSections = { running: true, recent: true, files: false };

function read(): RailSections {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return { ...DEFAULTS };
    const parsed = JSON.parse(raw) as Partial<RailSections>;
    return {
      running: typeof parsed.running === "boolean" ? parsed.running : DEFAULTS.running,
      recent: typeof parsed.recent === "boolean" ? parsed.recent : DEFAULTS.recent,
      files: typeof parsed.files === "boolean" ? parsed.files : DEFAULTS.files,
    };
  } catch {
    return { ...DEFAULTS };
  }
}

const [store, setStore] = createStore<RailSections>(read());

export { store as railSections };

export function setRailSection(key: keyof RailSections, value: boolean): void {
  setStore(key, value);
  try {
    localStorage.setItem(
      KEY,
      JSON.stringify({ running: store.running, recent: store.recent, files: store.files }),
    );
  } catch {
    // Private mode or a full quota: the toggle still works, just does not
    // survive a remount.
  }
}

/** Re-read from (cleared) localStorage. The store is module singleton state,
 *  same as `bookmarks.ts` — a test file's own `localStorage.clear()` leaves
 *  this in-memory value untouched unless something re-reads it. */
export function resetRailSections(): void {
  setStore(read());
}
