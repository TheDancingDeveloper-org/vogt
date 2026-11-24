// Height of the running-session list inside the places rail (WI-1664).
// Same per-device posture as every other layout preference (`layout.ts`,
// `resizablePane.ts`): localStorage, nothing shared, nothing lost on reload.
// A missing store (private mode, a full quota) still resizes for this session.

import { createSignal, onCleanup } from "solid-js";

export const SESSION_AREA_HEIGHT_KEY = "vogt.pane.places-session-area.height.v1";

/** About three session rows plus the "Running" toggle. */
export const SESSION_AREA_MIN_HEIGHT = 132;

/** Shipped height: the area keeps flexing to fill leftover rail space. */
export const SESSION_AREA_DEFAULT_HEIGHT = 0;

export interface SessionAreaHeight {
  /** 0 means "auto" — flex to fill the leftover column, as the rail shipped. */
  height: () => number;
  dragging: () => boolean;
  beginResize: (event: PointerEvent, startHeight: number, maxHeight: number) => void;
  setHeight: (next: number, maxHeight: number) => void;
  reset: () => void;
}

function readHeight(fallback: number): number {
  try {
    const raw = localStorage.getItem(SESSION_AREA_HEIGHT_KEY);
    const parsed = raw === null ? NaN : Number.parseInt(raw, 10);
    if (Number.isFinite(parsed) && parsed >= 0) return parsed;
  } catch {
    // localStorage unavailable — the in-memory default still works.
  }
  return fallback;
}

function persist(value: number): void {
  try {
    if (value <= SESSION_AREA_DEFAULT_HEIGHT) {
      localStorage.removeItem(SESSION_AREA_HEIGHT_KEY);
    } else {
      localStorage.setItem(SESSION_AREA_HEIGHT_KEY, String(value));
    }
  } catch {
    // Private mode or a full quota: the session still resizes.
  }
}

function clamp(next: number, maxHeight: number): number {
  const max = Math.max(SESSION_AREA_MIN_HEIGHT, maxHeight);
  return Math.min(max, Math.max(SESSION_AREA_MIN_HEIGHT, Math.round(next)));
}

export function createSessionAreaHeight(): SessionAreaHeight {
  const [height, setHeightSignal] = createSignal(readHeight(SESSION_AREA_DEFAULT_HEIGHT));
  const [dragging, setDragging] = createSignal(false);

  // One cleanup, registered at hook creation: a drag in progress leaves its
  // teardown here so a route change mid-drag still drops the window listeners.
  let stopActiveDrag: (() => void) | null = null;
  onCleanup(() => stopActiveDrag?.());

  const setHeight = (next: number, maxHeight: number) => {
    const clamped = next <= SESSION_AREA_DEFAULT_HEIGHT
      ? SESSION_AREA_DEFAULT_HEIGHT
      : clamp(next, maxHeight);
    setHeightSignal(clamped);
    persist(clamped);
  };

  const beginResize = (event: PointerEvent, startHeight: number, maxHeight: number) => {
    if (event.button !== 0) return;
    event.preventDefault();
    const origin = event.clientY;
    const base = startHeight < SESSION_AREA_MIN_HEIGHT ? SESSION_AREA_MIN_HEIGHT : startHeight;
    setDragging(true);

    const onMove = (moveEvent: PointerEvent) => {
      setHeightSignal(clamp(base + (moveEvent.clientY - origin), maxHeight));
    };
    const stop = () => {
      setDragging(false);
      persist(height());
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", stop);
      stopActiveDrag = null;
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", stop, { once: true });
    stopActiveDrag = stop;
  };

  return {
    height,
    dragging,
    beginResize,
    setHeight,
    reset: () => setHeight(SESSION_AREA_DEFAULT_HEIGHT, SESSION_AREA_MIN_HEIGHT),
  };
}
