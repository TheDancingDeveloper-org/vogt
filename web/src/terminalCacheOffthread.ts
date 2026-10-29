/**
 * Post a terminal-cache write to a worker. Falls back to the same put on the
 * main thread when workers are unavailable (jsdom, old WebViews).
 */

import { saveTerminalCache, type TerminalCachePayload } from "./terminalCache";
import type { TerminalCacheWrite } from "./terminalCacheWorker";

type CacheWorker = {
  postMessage: (message: TerminalCacheWrite) => void;
  terminate?: () => void;
};

let worker: CacheWorker | null | undefined;

function createWorker(): CacheWorker | null {
  if (typeof Worker === "undefined") return null;
  try {
    return new Worker(new URL("./terminalCache.worker.ts", import.meta.url), {
      type: "module",
    });
  } catch {
    return null;
  }
}

export function postTerminalCacheWrite(
  sessionId: string,
  outputPosition: number,
  payload: TerminalCachePayload,
  spawn: () => CacheWorker | null = createWorker,
): void {
  if (worker === undefined) worker = spawn();
  const message: TerminalCacheWrite = { sessionId, outputPosition, payload };
  if (worker) {
    try {
      worker.postMessage(message);
      return;
    } catch {
      worker = null;
    }
  }
  void saveTerminalCache(sessionId, outputPosition, payload);
}

const UNSET = Symbol("unset");

/** Tests replace or drop the singleton. Omit `next` to force a fresh spawn. */
export function resetTerminalCacheWorker(next: CacheWorker | null | typeof UNSET = UNSET): void {
  worker?.terminate?.();
  worker = next === UNSET ? undefined : next;
}
