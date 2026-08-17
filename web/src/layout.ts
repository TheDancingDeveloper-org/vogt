// Pane arrangement preference storage. Product places are routes; this only
// controls how the remaining editor panes arrange themselves.
const LAYOUT_MODE_KEY = "mydevenv2.layoutMode.v1";

export type LayoutMode = "tabbed" | "ide";

export function getLayoutMode(): LayoutMode {
  try {
    const stored = localStorage.getItem(LAYOUT_MODE_KEY);
    if (stored === "ide" || stored === "tabbed") return stored;
  } catch {
    // localStorage unavailable
  }
  return "tabbed"; // default
}

export function setLayoutMode(mode: LayoutMode) {
  try {
    localStorage.setItem(LAYOUT_MODE_KEY, mode);
  } catch {
    // localStorage unavailable - ignore
  }
}

/** Places keep the old tab snapshot around for one release while migrating. */
export const PLACES_STATE_KEY = "mydevenv2.places.v1";
export const PLACES_MIGRATION_KEY = "mydevenv2.places.migrated.v1";
