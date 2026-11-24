/**
 * Terminal renderer choice (WI-2694).
 *
 * The WebGL addon paints the grid on a canvas. That is faster for a busy
 * pane, but the canvas has no `.xterm-rows` text, and some hosts have no
 * working WebGL context: Android WebView, and the headless browser CI uses
 * for screenshot baselines. Those hosts, and anyone who turns the setting
 * off, stay on xterm's DOM renderer.
 *
 * A lost WebGL context disposes the addon and falls back to DOM for that
 * pane. The preference is not rewritten: the next pane may still try WebGL.
 */

export const TERMINAL_RENDERER_STORAGE_KEY = "vogt.terminalRenderer.v1";
export const TERMINAL_RENDERER_EVENT = "vogt:terminal-renderer";

export type TerminalRendererPreference = "webgl" | "dom";

export function readTerminalRenderer(): TerminalRendererPreference {
  try {
    return localStorage.getItem(TERMINAL_RENDERER_STORAGE_KEY) === "dom"
      ? "dom"
      : "webgl";
  } catch {
    return "webgl";
  }
}

export function writeTerminalRenderer(preference: TerminalRendererPreference): void {
  try {
    localStorage.setItem(TERMINAL_RENDERER_STORAGE_KEY, preference);
  } catch {
    /* storage can be unavailable in private / locked-down contexts */
  }
  window.dispatchEvent(
    new CustomEvent(TERMINAL_RENDERER_EVENT, { detail: { renderer: preference } }),
  );
}

/**
 * Whether this pane should try the WebGL addon.
 *
 * `dom` is an explicit user choice. `webgl` still yields to a host that has
 * no WebGL at all, so a screenshot run and an Android WebView never attach
 * an addon that cannot start.
 */
export function shouldUseWebglRenderer(
  preference: TerminalRendererPreference = readTerminalRenderer(),
  webglAvailable: boolean = isWebglAvailable(),
): boolean {
  return preference === "webgl" && webglAvailable;
}

export function isWebglAvailable(): boolean {
  try {
    const canvas = document.createElement("canvas");
    return Boolean(
      canvas.getContext("webgl2") || canvas.getContext("webgl"),
    );
  } catch {
    return false;
  }
}

export interface WebglTerminalAddon {
  activate(terminal: unknown): void;
  dispose(): void;
  onContextLoss(listener: () => void): void;
}

export interface WebglTerminal {
  loadAddon(addon: WebglTerminalAddon): void;
}

/**
 * Load the WebGL addon onto one terminal.
 *
 * Returns a dispose function, or `null` when the preference forces DOM, the
 * host has no WebGL, or the addon fails to start. On context loss the addon
 * is disposed and `onFallback` runs so the pane keeps painting with DOM.
 */
export async function attachWebglRenderer(
  term: WebglTerminal,
  options: {
    preference?: TerminalRendererPreference;
    webglAvailable?: boolean;
    loadAddon?: () => Promise<new () => WebglTerminalAddon>;
    onFallback?: (reason: "unavailable" | "context-loss" | "load-failed") => void;
  } = {},
): Promise<(() => void) | null> {
  const preference = options.preference ?? readTerminalRenderer();
  const available = options.webglAvailable ?? isWebglAvailable();
  if (!shouldUseWebglRenderer(preference, available)) {
    options.onFallback?.("unavailable");
    return null;
  }
  let Addon: new () => WebglTerminalAddon;
  try {
    const load =
      options.loadAddon ??
      (async () => {
        const mod = await import("@xterm/addon-webgl");
        return mod.WebglAddon;
      });
    Addon = await load();
  } catch {
    options.onFallback?.("load-failed");
    return null;
  }
  const addon = new Addon();
  let disposed = false;
  const dispose = () => {
    if (disposed) return;
    disposed = true;
    try {
      addon.dispose();
    } catch {
      /* already disposed by xterm */
    }
  };
  addon.onContextLoss(() => {
    dispose();
    options.onFallback?.("context-loss");
  });
  try {
    term.loadAddon(addon);
  } catch {
    dispose();
    options.onFallback?.("load-failed");
    return null;
  }
  return dispose;
}
