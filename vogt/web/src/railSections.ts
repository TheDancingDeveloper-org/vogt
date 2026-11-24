// Which places-rail sections are open, shared across the rail's own session
// sections, Recent places and FileTree's Files header — one JSON object so
// a write from either side cannot drop the other's field (rail-spec.md B3).
import { createStore } from "solid-js/store";

const KEY = "vogt.rail.sections.v1";

export interface RailSections {
  /** The Pinned strip of bookmarked sessions (WI-1736). */
  pinned: boolean;
  /** Session sections, one per state (WI-1736). */
  needs: boolean;
  running: boolean;
  ready: boolean;
  done: boolean;
  older: boolean;
  recent: boolean;
  files: boolean;
  /** Show sessions the engine hides from the default list (WI-1289). */
  showHidden: boolean;
}

const DEFAULTS: RailSections = {
  pinned: true,
  needs: true,
  running: true,
  ready: true,
  done: true,
  // Finished work past the retain window: out of the way until asked for.
  older: false,
  recent: true,
  files: false,
  showHidden: false,
};

const KEYS = Object.keys(DEFAULTS) as (keyof RailSections)[];

function read(): RailSections {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return { ...DEFAULTS };
    const parsed = JSON.parse(raw) as Partial<RailSections>;
    const next = { ...DEFAULTS };
    for (const key of KEYS) {
      if (typeof parsed[key] === "boolean") next[key] = parsed[key] as boolean;
    }
    return next;
  } catch {
    return { ...DEFAULTS };
  }
}

const [store, setStore] = createStore<RailSections>(read());

export { store as railSections };

export function setRailSection(key: keyof RailSections, value: boolean): void {
  setStore(key, value);
  try {
    const saved: Partial<RailSections> = {};
    for (const name of KEYS) saved[name] = store[name];
    localStorage.setItem(KEY, JSON.stringify(saved));
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
