/** The single, debounced foreground entry point for the PWA. */

export type WakeReason =
  | "boot"
  | "visibility"
  | "focus"
  | "resume"
  | "sse-reconnect"
  | "manual";

export interface Wake {
  token: number;
  reason: WakeReason;
  at: number;
  /**
   * How long the page had been hidden before this wake, in ms: the longest
   * hidden spell ended inside the burst, or 0 when nothing in it followed a
   * hidden page (a bare window focus, a stream note). Lets a listener tell a
   * real return to the front from a focus edge that moved between frames.
   */
  hiddenMs: number;
}

export type WakeOutcome = "ok" | "abort" | "error";

export interface WakeResourceTelemetry {
  key: string;
  startedMs: number;
  durationMs: number;
  outcome: WakeOutcome;
}

export interface WakeTelemetry {
  token: number;
  reason: WakeReason;
  at: number;
  resources: WakeResourceTelemetry[];
}

const DEBOUNCE_MS = 250;
const LOG_LIMIT = 50;
let nextToken = 0;
let pendingReason: WakeReason | null = null;
let pendingHiddenMs = 0;
/** When the page last went hidden, or null while it is visible. */
let hiddenSince: number | null = null;
let timer: ReturnType<typeof setTimeout> | null = null;
let latest: Wake | null = null;

const listeners = new Set<(wake: Wake) => void>();
const visibilityListeners = new Set<(visible: boolean) => void>();
const telemetry: WakeTelemetry[] = [];
const inFlight = new Map<string, {
  token: number;
  controller: AbortController;
  promise: Promise<unknown>;
}>();

function visible(): boolean {
  return typeof document === "undefined" || document.visibilityState === "visible";
}

function emit(): void {
  timer = null;
  const reason = pendingReason;
  const hiddenMs = pendingHiddenMs;
  pendingReason = null;
  pendingHiddenMs = 0;
  if (!reason || !visible()) return;

  const wake: Wake = { token: ++nextToken, reason, at: Date.now(), hiddenMs };
  latest = wake;
  telemetry.push({ token: wake.token, reason, at: wake.at, resources: [] });
  if (telemetry.length > LOG_LIMIT) telemetry.shift();
  for (const listener of listeners) listener(wake);
}

/**
 * Schedule one foreground wake for the current burst of lifecycle events.
 *
 * The first reason names the burst, with one exception: `sse-reconnect` is
 * the event stream noting its own loss, not the app coming forward, so a real
 * lifecycle signal in the same burst replaces it. Listeners that act only on
 * a genuine return-to-front (the forced stream reconnect) key on the reason.
 */
export function noteForeground(reason: WakeReason, hiddenMs = 0): void {
  if (!visible()) return;
  pendingHiddenMs = Math.max(pendingHiddenMs, hiddenMs);
  if (
    pendingReason === null
    || (pendingReason === "sse-reconnect" && reason !== "sse-reconnect")
  ) {
    pendingReason = reason;
  }
  if (timer === null) timer = setTimeout(emit, DEBOUNCE_MS);
}

export function currentWake(): Wake | null {
  return latest;
}

export function onWake(listener: (wake: Wake) => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Share page visibility edges without giving every retained pane a DOM listener. */
export function onPageVisibility(listener: (visible: boolean) => void): () => void {
  visibilityListeners.add(listener);
  listener(visible());
  return () => visibilityListeners.delete(listener);
}

/** Share a resource read during one wake and record its client-side timing. */
export function reconcile<T>(
  key: string,
  wake: Wake,
  run: (signal: AbortSignal) => Promise<T>,
): Promise<T> {
  const existing = inFlight.get(key);
  if (existing?.token === wake.token) return existing.promise as Promise<T>;
  if (existing) existing.controller.abort();

  const controller = new AbortController();
  const startedMs = performance.now();
  const record = (outcome: WakeOutcome) => {
    const entry = telemetry.find((candidate) => candidate.token === wake.token);
    entry?.resources.push({
      key,
      startedMs,
      durationMs: Math.max(0, performance.now() - startedMs),
      outcome,
    });
  };
  const promise = run(controller.signal).then(
    (value) => { record("ok"); return value; },
    (error: unknown) => {
      record(error instanceof DOMException && error.name === "AbortError" ? "abort" : "error");
      throw error;
    },
  );
  inFlight.set(key, { token: wake.token, controller, promise });
  void promise.then(
    () => { if (inFlight.get(key)?.promise === promise) inFlight.delete(key); },
    () => { if (inFlight.get(key)?.promise === promise) inFlight.delete(key); },
  );
  return promise;
}

/**
 * Drop a wake that is still inside its debounce window. Tests only: the
 * coordinator is module state, so a wake a test noted just before it ended
 * would otherwise fire inside the next test, against surfaces that test
 * mounted (the live.test backlog flake, WI-1886).
 */
export function cancelPendingWakeForTests(): void {
  if (timer !== null) clearTimeout(timer);
  timer = null;
  pendingReason = null;
  pendingHiddenMs = 0;
}

/** Last fifty wakes, useful when diagnosing a resume stall from the console. */
export const wakeLog: ReadonlyArray<WakeTelemetry> = telemetry;

if (typeof document !== "undefined") {
  if (!visible()) hiddenSince = Date.now();
  document.addEventListener("visibilitychange", () => {
    const isVisible = document.visibilityState === "visible";
    for (const listener of visibilityListeners) listener(isVisible);
    if (!isVisible) {
      hiddenSince ??= Date.now();
      return;
    }
    const hiddenMs = hiddenSince === null ? 0 : Date.now() - hiddenSince;
    hiddenSince = null;
    noteForeground("visibility", hiddenMs);
  });
}

if (typeof window !== "undefined") {
  window.addEventListener("focus", () => noteForeground("focus"));
  void (async () => {
    try {
      const { App } = await import("@capacitor/app");
      await App.addListener("resume", () => noteForeground("resume"));
    } catch {
      // Plain web/PWA deployments do not expose Capacitor.
    }
  })();
}
