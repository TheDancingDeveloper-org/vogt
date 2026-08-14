// Editor preferences stored in localStorage.
const MINIMAP_KEY = "mydevenv2.editor.minimap.v1";

export function getMinimapEnabled(): boolean {
  try {
    return localStorage.getItem(MINIMAP_KEY) === "1";
  } catch {
    return false;
  }
}

export function setMinimapEnabled(enabled: boolean) {
  try {
    localStorage.setItem(MINIMAP_KEY, enabled ? "1" : "0");
  } catch {
    /* non-fatal */
  }
}
