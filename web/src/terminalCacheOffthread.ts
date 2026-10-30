/**
 * Post a terminal-cache write to a worker. Falls back to the same put on the
 * main thread when workers are unavailable (jsdom, old WebViews) or the worker
 * fails (module load error, messageerror): pending writes are replayed on the
 * main thread and every later write goes there too.
 *
 * The returned promise resolves with the save result: `written`, `stale` (the
 * store already held this or a later offset of the same stream) or `failed`.
 * Whether `stale` counts as saved is the caller's call.
 */

import { saveTerminalCache, type TerminalCachePayload, type TerminalCacheSaveResult } from "./terminalCache";
import type { TerminalCacheAck, TerminalCacheWrite } from "./terminalCacheWorker";

export type CacheWorker = {
  postMessage: (message: TerminalCacheWrite) => void;
  terminate?: () => void;
  onmessage: ((ev: MessageEvent<TerminalCacheAck>) => void) | null;
  onerror: ((ev: Event) => void) | null;
  onmessageerror: ((ev: MessageEvent) => void) | null;
};

interface Pending {
  message: TerminalCacheWrite;
  resolve: (result: TerminalCacheSaveResult) => void;
}

let worker: CacheWorker | null | undefined;
let nextId = 1;
const pending = new Map<number, Pending>();

function writeOnMainThread(entry: Pending): void {
  const { sessionId, outputPosition, payload, replace } = entry.message;
  void saveTerminalCache(sessionId, outputPosition, payload, replace === true).then(entry.resolve);
}

function abandonWorker(reason: string): void {
  if (!worker) return;
  console.warn("[vogt] terminal cache worker failed; writing on the main thread", { reason });
  worker.terminate?.();
  worker = null;
  const replay = [...pending.values()];
  pending.clear();
  for (const entry of replay) writeOnMainThread(entry);
}

function wire(w: CacheWorker): CacheWorker {
  w.onmessage = (ev) => {
    const entry = pending.get(ev.data?.id);
    if (!entry) return;
    pending.delete(ev.data.id);
    entry.resolve(ev.data.result);
  };
  w.onerror = () => abandonWorker("error");
  w.onmessageerror = () => abandonWorker("messageerror");
  return w;
}

function createWorker(): CacheWorker | null {
  if (typeof Worker === "undefined") return null;
  try {
    return new Worker(new URL("./terminalCache.worker.ts", import.meta.url), {
      type: "module",
    }) as unknown as CacheWorker;
  } catch {
    return null;
  }
}

export function postTerminalCacheWrite(
  sessionId: string,
  outputPosition: number,
  payload: TerminalCachePayload,
  spawn: () => CacheWorker | null = createWorker,
  direct = false,
  replace = false,
): Promise<TerminalCacheSaveResult> {
  return new Promise<TerminalCacheSaveResult>((resolve) => {
    const entry: Pending = {
      message: { id: nextId++, sessionId, outputPosition, payload, ...(replace ? { replace } : {}) },
      resolve,
    };
    // `direct` is the pagehide path: a worker can be torn down with the page
    // before its put commits, so the write starts on this thread instead.
    if (direct) {
      writeOnMainThread(entry);
      return;
    }
    if (worker === undefined) {
      const spawned = spawn();
      worker = spawned ? wire(spawned) : null;
    }
    if (worker) {
      try {
        pending.set(entry.message.id, entry);
        worker.postMessage(entry.message);
        return;
      } catch {
        pending.delete(entry.message.id);
        abandonWorker("postMessage threw");
      }
    }
    writeOnMainThread(entry);
  });
}

const UNSET = Symbol("unset");

/** Tests replace or drop the singleton. Omit `next` to force a fresh spawn. */
export function resetTerminalCacheWorker(next: CacheWorker | null | typeof UNSET = UNSET): void {
  worker?.terminate?.();
  pending.clear();
  worker = next === UNSET ? undefined : next ? wire(next) : null;
}
