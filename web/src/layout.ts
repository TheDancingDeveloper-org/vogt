// Layout mode preference storage
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
