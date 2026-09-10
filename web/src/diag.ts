// Client diagnostics: the PWA's own account of what it did, posted to the
// engine so it lands on the server's log stream (`POST /api/client-log`).
//
// The engine can see that a phone asked for a spoken reply and got a 200; it
// cannot see what the WebView did next — whether `audio.play()` resolved or
// rejected and with what, whether the element fired `ended` at once, what the
// Blob's type was, what the hands-free machine was doing. Every one of those
// was being inferred from the server side. This module makes them a log line
// an operator reads with `docker logs` / Komodo, next to the request they
// belong to.
//
// Cheap and safe by construction: events are batched (a flush every 2 s or at
// 20 events, and on page hide), values are clipped, and callers pass lengths
// and names — never transcript text. A failed post is dropped silently;
// diagnostics must never become the bug. On by default so a field report
// carries its evidence; `localStorage["vogt.diag"] = "0"` switches it off.

import { api } from "./api";

const FLAG = "vogt.diag";
const FLUSH_MS = 2000;
const BATCH = 20;
/** Matches the engine's per-batch cap. */
const MAX_PER_POST = 64;

export interface DiagEvent {
  t: number;
  event: string;
  fields: Record<string, unknown>;
}

let queue: DiagEvent[] = [];
let timer: ReturnType<typeof setTimeout> | null = null;
let booted = false;

export function diagEnabled(): boolean {
  try {
    return localStorage.getItem(FLAG) !== "0";
  } catch {
    return true;
  }
}

/** Clip every value so a stray object cannot bloat a batch past the engine's cap. */
function sanitize(fields: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(fields)) {
    if (value === null || value === undefined) continue;
    if (typeof value === "number" || typeof value === "boolean") {
      out[key] = value;
      continue;
    }
    let text: string;
    if (typeof value === "string") text = value;
    else {
      try {
        text = JSON.stringify(value) ?? String(value);
      } catch {
        text = String(value);
      }
    }
    out[key] = text.length > 200 ? `${text.slice(0, 200)}…` : text;
  }
  return out;
}

/** Record one event. Batched; never throws. */
export function diag(event: string, fields: Record<string, unknown> = {}): void {
  if (!diagEnabled()) return;
  queue.push({ t: Date.now(), event, fields: sanitize(fields) });
  if (queue.length >= BATCH) flush();
  else if (timer === null) timer = setTimeout(flush, FLUSH_MS);
}

/** Post what is queued now. Safe to call at any time. */
export function flush(): void {
  if (timer !== null) {
    clearTimeout(timer);
    timer = null;
  }
  const batch = queue.splice(0, MAX_PER_POST);
  if (batch.length === 0) return;
  void api.clientLog(batch).catch(() => {
    /* diagnostics never break the app */
  });
  if (queue.length > 0) timer = setTimeout(flush, 0);
}

/** Once per page: the environment facts a bug report needs, and flush hooks. */
export function diagBoot(): void {
  if (booted) return;
  booted = true;
  const nav = navigator as Navigator & {
    userActivation?: { isActive: boolean; hasBeenActive: boolean };
  };
  diag("app.boot", {
    ua: navigator.userAgent,
    synthInWindow: "speechSynthesis" in window,
    mediaRecorder: typeof MediaRecorder !== "undefined",
    userActivated: nav.userActivation?.hasBeenActive ?? null,
    visibility: document.visibilityState,
  });
  window.addEventListener("pagehide", flush);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") flush();
  });
}

/** Test seam: drop queued state. */
export function resetDiagForTests(): void {
  queue = [];
  if (timer !== null) clearTimeout(timer);
  timer = null;
  booted = false;
}
