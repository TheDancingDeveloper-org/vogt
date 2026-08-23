/** App (shell) colour themes — the sibling of `terminalThemes.ts` for the
 * whole PWA rather than just the terminal (#299).
 *
 * A theme is a token set defined in `styles.css` under a `[data-theme="…"]`
 * block; this module is the selection/persistence/broadcast layer over it. The
 * selected value is stored under a `vogt.`-prefixed key (the product's current
 * identity); a legacy `mydevenv2.`-prefixed value is read once as a fallback so
 * an upgrade does not discard the reader's choice.
 *
 * "System" follows the OS via `prefers-color-scheme`. Every named theme also
 * carries the terminal preset and Monaco theme that match it, so the coupled
 * surfaces default sensibly when the shell theme changes (an explicit terminal
 * choice still wins — see `terminalThemes.getThemeName`).
 */

export type ThemeBase = "dark" | "light";

export interface AppTheme {
  /** The `data-theme` attribute value and stored id. */
  id: string;
  /** Human label shown in the picker. */
  label: string;
  /** Whether the palette is fundamentally light or dark. */
  base: ThemeBase;
  /** The built-in Monaco theme that matches this palette. */
  monaco: "vs" | "vs-dark" | "hc-black" | "hc-light";
  /** The terminal preset (a key of `terminalThemes.THEMES`) that matches. */
  terminal: string;
}

/** The special "follow the OS" selection. Not a real `data-theme`. */
export const SYSTEM_SELECTION = "system";

export const APP_THEMES: Record<string, AppTheme> = {
  dark: { id: "dark", label: "Vogt Dark", base: "dark", monaco: "vs-dark", terminal: "GitHub Dark" },
  dim: { id: "dim", label: "Vogt Dim", base: "dark", monaco: "vs-dark", terminal: "One Dark" },
  light: { id: "light", label: "Vogt Light", base: "light", monaco: "vs", terminal: "GitHub Light" },
  "hc-dark": {
    id: "hc-dark", label: "High contrast (dark)", base: "dark",
    monaco: "hc-black", terminal: "GitHub Dark",
  },
  "hc-light": {
    id: "hc-light", label: "High contrast (light)", base: "light",
    monaco: "hc-light", terminal: "GitHub Light",
  },
};

/** The order the picker lists them in: System first, then dark→light. */
export const APP_THEME_ORDER = ["dark", "dim", "hc-dark", "light", "hc-light"] as const;

const SELECTION_KEY = "vogt.appTheme.v1";
const LEGACY_SELECTION_KEY = "mydevenv2.appTheme.v1";
const APP_THEME_EVENT = "vogt:app-theme";

export const APP_THEME_STORAGE_KEY = SELECTION_KEY;
export const APP_THEME_CHANGE_EVENT = APP_THEME_EVENT;

/** True for `system` or any known theme id; anything else is discarded. */
function isValidSelection(value: string | null): value is string {
  return value === SYSTEM_SELECTION || (value != null && value in APP_THEMES);
}

/**
 * The stored selection (`system` or a theme id), reading the legacy key once as
 * a fallback and adopting it under the current key so the next read is direct.
 */
export function getAppThemeSelection(): string {
  try {
    const current = localStorage.getItem(SELECTION_KEY);
    if (isValidSelection(current)) return current;
    const legacy = localStorage.getItem(LEGACY_SELECTION_KEY);
    if (isValidSelection(legacy)) {
      try {
        localStorage.setItem(SELECTION_KEY, legacy);
      } catch {
        /* non-fatal: still return what we read */
      }
      return legacy;
    }
  } catch {
    /* localStorage unavailable (private mode, disabled) */
  }
  return SYSTEM_SELECTION;
}

/** Does the OS currently ask for a dark UI? */
export function prefersDark(): boolean {
  try {
    return (
      typeof window !== "undefined"
      && typeof window.matchMedia === "function"
      && window.matchMedia("(prefers-color-scheme: dark)").matches
    );
  } catch {
    return true; // Vogt's default look is dark.
  }
}

/** Resolve a selection to a concrete theme id (`system` → OS preference). */
export function resolveTheme(selection: string = getAppThemeSelection()): string {
  if (selection === SYSTEM_SELECTION) return prefersDark() ? "dark" : "light";
  return selection in APP_THEMES ? selection : "dark";
}

/** The resolved active theme's descriptor. */
export function getActiveTheme(): AppTheme {
  return APP_THEMES[resolveTheme()]!;
}

/**
 * Point the PWA `theme-color` meta at the active surface colour, so the
 * browser address bar and (via the platform) the native status bar follow the
 * theme. Reads the live `--bg` token, so it needs no per-theme table here.
 */
function syncThemeColorMeta(): void {
  if (typeof document === "undefined") return;
  const bg = getComputedStyle(document.documentElement).getPropertyValue("--bg").trim();
  if (!bg) return;
  let meta = document.querySelector('meta[name="theme-color"]');
  if (!meta) {
    meta = document.createElement("meta");
    meta.setAttribute("name", "theme-color");
    document.head.appendChild(meta);
  }
  meta.setAttribute("content", bg);
}

/**
 * Apply the current selection to `<html data-theme>` and refresh the coupled
 * surfaces. Broadcasts on the app-theme channel and re-fires the terminal
 * channel so open terminals re-read their (possibly app-derived) preset.
 */
export function applyAppTheme(selection: string = getAppThemeSelection()): string {
  const themeId = resolveTheme(selection);
  if (typeof document !== "undefined") {
    document.documentElement.setAttribute("data-theme", themeId);
    syncThemeColorMeta();
  }
  if (typeof window !== "undefined") {
    window.dispatchEvent(
      new CustomEvent(APP_THEME_EVENT, { detail: { selection, themeId } }),
    );
    // Terminals and editors follow the shell unless the user pinned their own.
    window.dispatchEvent(new CustomEvent("vogt:terminal-theme", { detail: {} }));
  }
  return themeId;
}

/** Persist a selection and apply it immediately. */
export function setAppTheme(selection: string): string {
  const next = isValidSelection(selection) ? selection : SYSTEM_SELECTION;
  try {
    localStorage.setItem(SELECTION_KEY, next);
  } catch {
    /* non-fatal */
  }
  return applyAppTheme(next);
}

/** The terminal preset that matches the active shell theme. */
export function activeTerminalPreset(): string {
  return getActiveTheme().terminal;
}

/** The Monaco theme that matches the active shell theme. */
export function activeMonacoTheme(): string {
  return getActiveTheme().monaco;
}

let systemListenerBound = false;

/**
 * Start following the OS when the selection is "System". Idempotent. Call once
 * at boot; re-applies the theme whenever `prefers-color-scheme` flips and the
 * user is on System.
 */
export function initAppThemeSystemWatch(): void {
  if (systemListenerBound || typeof window === "undefined") return;
  if (typeof window.matchMedia !== "function") return;
  systemListenerBound = true;
  const mql = window.matchMedia("(prefers-color-scheme: dark)");
  const onChange = () => {
    if (getAppThemeSelection() === SYSTEM_SELECTION) applyAppTheme(SYSTEM_SELECTION);
  };
  if (typeof mql.addEventListener === "function") {
    mql.addEventListener("change", onChange);
  } else if (typeof (mql as MediaQueryList).addListener === "function") {
    (mql as MediaQueryList).addListener(onChange);
  }
}
