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
let timer: ReturnType<typeof setTimeout> | null = null;
let latest: Wake | null = null;

const listeners = new Set<(wake: Wake) => void>();
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
  pendingReason = null;
  if (!reason || !visible()) return;

  const wake: Wake = { token: ++nextToken, reason, at: Date.now() };
  latest = wake;
  telemetry.push({ token: wake.token, reason, at: wake.at, resources: [] });
  if (telemetry.length > LOG_LIMIT) telemetry.shift();
  for (const listener of listeners) listener(wake);
}

/** Schedule one foreground wake for the current burst of lifecycle events. */
export function noteForeground(reason: WakeReason): void {
  if (!visible()) return;
  if (pendingReason === null) pendingReason = reason;
  if (timer === null) timer = setTimeout(emit, DEBOUNCE_MS);
}

export function currentWake(): Wake | null {
  return latest;
}

export function onWake(listener: (wake: Wake) => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
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

/** Last fifty wakes, useful when diagnosing a resume stall from the console. */
export const wakeLog: ReadonlyArray<WakeTelemetry> = telemetry;

if (typeof document !== "undefined") {
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") noteForeground("visibility");
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
