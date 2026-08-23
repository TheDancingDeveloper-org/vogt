// Terminal color theme presets. Each theme is an xterm ITheme-compatible object.

import { activeTerminalPreset } from "./appThemes";

export interface TerminalTheme {
  background: string;
  foreground: string;
  cursor: string;
  selectionBackground: string;
  black: string;
  red: string;
  green: string;
  yellow: string;
  blue: string;
  magenta: string;
  cyan: string;
  white: string;
  brightBlack: string;
  brightRed: string;
  brightGreen: string;
  brightYellow: string;
  brightBlue: string;
  brightMagenta: string;
  brightCyan: string;
  brightWhite: string;
}

export const THEMES: Record<string, TerminalTheme> = {
  "GitHub Dark": {
    background: "#000000",
    foreground: "#c9d1d9",
    cursor: "#58a6ff",
    selectionBackground: "#1f6feb55",
    black: "#484f58",
    red: "#ff7b72",
    green: "#7ee787",
    yellow: "#d29922",
    blue: "#58a6ff",
    magenta: "#bc8cff",
    cyan: "#39c5cf",
    white: "#c9d1d9",
    brightBlack: "#6e7681",
    brightRed: "#ffa198",
    brightGreen: "#56d364",
    brightYellow: "#e3b341",
    brightBlue: "#79c0ff",
    brightMagenta: "#d2a8ff",
    brightCyan: "#56d4dd",
    brightWhite: "#f0f6fc",
  },
  Dracula: {
    background: "#282a36",
    foreground: "#f8f8f2",
    cursor: "#f8f8f2",
    selectionBackground: "#44475a",
    black: "#21222c",
    red: "#ff5555",
    green: "#50fa7b",
    yellow: "#f1fa8c",
    blue: "#bd93f9",
    magenta: "#ff79c6",
    cyan: "#8be9fd",
    white: "#f8f8f2",
    brightBlack: "#6272a4",
    brightRed: "#ff6e6e",
    brightGreen: "#69ff94",
    brightYellow: "#ffffa5",
    brightBlue: "#d6acff",
    brightMagenta: "#ff92df",
    brightCyan: "#a4ffff",
    brightWhite: "#ffffff",
  },
  "Solarized Dark": {
    background: "#002b36",
    foreground: "#839496",
    cursor: "#93a1a1",
    selectionBackground: "#073642",
    black: "#073642",
    red: "#dc322f",
    green: "#859900",
    yellow: "#b58900",
    blue: "#268bd2",
    magenta: "#d33682",
    cyan: "#2aa198",
    white: "#eee8d5",
    brightBlack: "#586e75",
    brightRed: "#cb4b16",
    brightGreen: "#586e75",
    brightYellow: "#657b83",
    brightBlue: "#839496",
    brightMagenta: "#6c71c4",
    brightCyan: "#93a1a1",
    brightWhite: "#fdf6e3",
  },
  "One Dark": {
    background: "#282c34",
    foreground: "#abb2bf",
    cursor: "#528bff",
    selectionBackground: "#3e4451",
    black: "#282c34",
    red: "#e06c75",
    green: "#98c379",
    yellow: "#e5c07b",
    blue: "#61afef",
    magenta: "#c678dd",
    cyan: "#56b6c2",
    white: "#abb2bf",
    brightBlack: "#5c6370",
    brightRed: "#e06c75",
    brightGreen: "#98c379",
    brightYellow: "#e5c07b",
    brightBlue: "#61afef",
    brightMagenta: "#c678dd",
    brightCyan: "#56b6c2",
    brightWhite: "#ffffff",
  },
  "GitHub Light": {
    background: "#ffffff",
    foreground: "#24292f",
    cursor: "#0969da",
    selectionBackground: "#0969da33",
    black: "#24292f",
    red: "#cf222e",
    green: "#116329",
    yellow: "#4d2d00",
    blue: "#0969da",
    magenta: "#8250df",
    cyan: "#1b7c83",
    white: "#6e7781",
    brightBlack: "#57606a",
    brightRed: "#a40e26",
    brightGreen: "#1a7f37",
    brightYellow: "#633c01",
    brightBlue: "#218bff",
    brightMagenta: "#a475f9",
    brightCyan: "#3192aa",
    brightWhite: "#8c959f",
  },
};

const THEME_KEY = "vogt.terminalTheme.v1";
const THEME_EVENT = "vogt:terminal-theme";

/** Has the reader pinned a terminal theme of their own? If so it wins over the
 *  app theme's default preset. */
export function hasExplicitTerminalTheme(): boolean {
  try {
    const stored = localStorage.getItem(THEME_KEY);
    return Boolean(stored && THEMES[stored]);
  } catch {
    return false;
  }
}

export function getThemeName(): string {
  try {
    const stored = localStorage.getItem(THEME_KEY);
    if (stored && THEMES[stored]) return stored;
  } catch {
    /* localStorage unavailable */
  }
  // No explicit choice: follow the shell theme's matching preset (#299), and
  // fall back to the historic default if that preset is somehow unknown.
  const coupled = activeTerminalPreset();
  return THEMES[coupled] ? coupled : "GitHub Dark";
}

export function getTheme(): TerminalTheme {
  return THEMES[getThemeName()]!;
}

export function setThemeName(name: string) {
  if (!THEMES[name]) return;
  try {
    localStorage.setItem(THEME_KEY, name);
  } catch {
    /* non-fatal */
  }
  window.dispatchEvent(new CustomEvent(THEME_EVENT, { detail: { name } }));
}

export const TERMINAL_THEME_EVENT = THEME_EVENT;
