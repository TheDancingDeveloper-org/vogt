// A side pane's width and open/closed state, drag-resizable and persisted
// per device — the same per-client posture every other layout preference in
// this product keeps (`layout.ts`, saved filters, the board's collapsed
// columns): nothing here is shared or synced, and nothing is lost on reload.
//
// One hook serves every resizable pane in the shell (the places rail, the
// Sessions live-list) rather than each screen inventing its own drag
// arithmetic and its own storage key shape.

import { createSignal, onCleanup } from "solid-js";

export interface ResizablePaneOptions {
  /** Distinguishes this pane's storage keys from every other pane's. */
  key: string;
  defaultWidth: number;
  min: number;
  max: number;
  defaultCollapsed?: boolean;
}

export interface ResizablePane {
  width: () => number;
  collapsed: () => boolean;
  setCollapsed: (value: boolean) => void;
  toggle: () => void;
  /** Bind to the drag handle's `onPointerDown`. */
  beginResize: (event: PointerEvent) => void;
  /** The keyboard equivalent of a drag — clamped and persisted the same way. */
  setWidth: (next: number) => void;
  dragging: () => boolean;
}

function storageKey(key: string, field: "width" | "collapsed"): string {
  return `vogt.pane.${key}.${field}.v1`;
}

function readWidth(key: string, fallback: number, min: number, max: number): number {
  try {
    const raw = localStorage.getItem(storageKey(key, "width"));
    const parsed = raw === null ? NaN : Number.parseInt(raw, 10);
    if (Number.isFinite(parsed)) return Math.min(max, Math.max(min, parsed));
  } catch {
    // localStorage unavailable — the in-memory default still works.
  }
  return fallback;
}

function readCollapsed(key: string, fallback: boolean): boolean {
  try {
    const raw = localStorage.getItem(storageKey(key, "collapsed"));
    if (raw === "1") return true;
    if (raw === "0") return false;
  } catch {
    // localStorage unavailable
  }
  return fallback;
}

export function createResizablePane(options: ResizablePaneOptions): ResizablePane {
  const { key, min, max } = options;
  const [width, setWidthSignal] = createSignal(
    readWidth(key, options.defaultWidth, min, max),
  );
  const [collapsed, setCollapsedSignal] = createSignal(
    readCollapsed(key, options.defaultCollapsed ?? false),
  );
  const [dragging, setDragging] = createSignal(false);

  const persistWidth = (value: number) => {
    try {
      localStorage.setItem(storageKey(key, "width"), String(value));
    } catch {
      // Private mode or a full quota: the session still resizes, just does
      // not remember it for the next one.
    }
  };

  const setCollapsed = (value: boolean) => {
    setCollapsedSignal(value);
    try {
      localStorage.setItem(storageKey(key, "collapsed"), value ? "1" : "0");
    } catch {
      // ditto
    }
  };

  // `onCleanup` has to run during the owning component's own setup to be
  // registered at all — called later, from inside a pointer-event handler,
  // Solid warns and drops it silently. So there is exactly one `onCleanup`
  // here, registered once at hook creation, and a drag in progress leaves
  // its own teardown in this variable for that one cleanup to find if the
  // component unmounts mid-drag (a route change while the reader is still
  // holding the handle).
  let stopActiveDrag: (() => void) | null = null;
  onCleanup(() => stopActiveDrag?.());

  const beginResize = (event: PointerEvent) => {
    // A drag never starts a collapsed pane's resize — there is nothing to
    // drag against — and the primary button only, so this does not eat a
    // right-click or a trackpad two-finger gesture aimed at something else.
    if (collapsed() || event.button !== 0) return;
    event.preventDefault();
    const startX = event.clientX;
    const startWidth = width();
    setDragging(true);

    const onMove = (moveEvent: PointerEvent) => {
      const next = Math.min(max, Math.max(min, startWidth + (moveEvent.clientX - startX)));
      setWidthSignal(next);
    };
    const stop = () => {
      setDragging(false);
      persistWidth(width());
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", stop);
      stopActiveDrag = null;
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", stop, { once: true });
    stopActiveDrag = stop;
  };

  const setWidth = (next: number) => {
    const clamped = Math.min(max, Math.max(min, next));
    setWidthSignal(clamped);
    persistWidth(clamped);
  };

  return {
    width,
    collapsed,
    setCollapsed,
    toggle: () => setCollapsed(!collapsed()),
    beginResize,
    setWidth,
    dragging,
  };
}
