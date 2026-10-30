/**
 * When the terminal cache is allowed to hit IndexedDB (WI-2695).
 *
 * The live ring still appends per frame. A persist is scheduled only when the
 * byte cursor moved, on the 5 s cadence, for visible and hidden panes alike: a
 * hidden pane still receives output, and its cache is what a reload restores.
 * Entering dormancy persists at once. The write itself is deferred to idle time
 * so serialize + the IDB put are not a long task on the frame that received the
 * bytes. A pending write is flushed on pagehide / visibilitychange so closing
 * the page does not drop it.
 */

export const CACHE_PERSIST_INTERVAL_MS = 5_000;
export const CACHE_PERSIST_IDLE_TIMEOUT_MS = 2_000;

export function cacheNeedsPersist(
  outputPosition: number | undefined,
  lastPersistedPosition: number | undefined,
): outputPosition is number {
  return outputPosition !== undefined && outputPosition !== lastPersistedPosition;
}

/** Schedule when the cursor moved and no persist is already pending. */
export function shouldScheduleCachePersist(input: {
  outputPosition: number | undefined;
  lastPersistedPosition: number | undefined;
  alreadyScheduled: boolean;
}): boolean {
  if (input.alreadyScheduled) return false;
  return cacheNeedsPersist(input.outputPosition, input.lastPersistedPosition);
}

export interface PageHideTarget {
  addEventListener: (type: string, cb: () => void) => void;
  removeEventListener: (type: string, cb: () => void) => void;
}

/**
 * Call `flush` when the page is hidden or unloaded. `pagehide` covers close
 * and bfcache; `visibilitychange` to hidden is the last reliable event on
 * mobile, where pagehide may never fire. Returns the uninstaller.
 */
export function installCacheFlushOnPageHide(
  flush: () => void,
  windowLike: PageHideTarget,
  documentLike: PageHideTarget & { visibilityState: string },
): () => void {
  const onPageHide = () => flush();
  const onVisibility = () => {
    if (documentLike.visibilityState === "hidden") flush();
  };
  windowLike.addEventListener("pagehide", onPageHide);
  documentLike.addEventListener("visibilitychange", onVisibility);
  return () => {
    windowLike.removeEventListener("pagehide", onPageHide);
    documentLike.removeEventListener("visibilitychange", onVisibility);
  };
}

export interface IdleWindow {
  requestIdleCallback?: (
    cb: () => void,
    opts?: { timeout: number },
  ) => number;
  cancelIdleCallback?: (handle: number) => void;
  setTimeout: (cb: () => void, ms: number) => ReturnType<typeof setTimeout>;
  clearTimeout: (handle: ReturnType<typeof setTimeout>) => void;
}

export interface IdleHandle {
  cancel: () => void;
}

/** Run `work` on the next idle slice, or after `timeoutMs` if idle never comes. */
export function scheduleOnIdle(
  work: () => void,
  windowLike: IdleWindow,
  timeoutMs: number = CACHE_PERSIST_IDLE_TIMEOUT_MS,
): IdleHandle {
  let done = false;
  let idleId: number | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  const run = () => {
    if (done) return;
    done = true;
    if (idleId !== null) windowLike.cancelIdleCallback?.(idleId);
    if (timer !== null) windowLike.clearTimeout(timer);
    work();
  };
  if (typeof windowLike.requestIdleCallback === "function") {
    idleId = windowLike.requestIdleCallback(run, { timeout: timeoutMs });
  }
  timer = windowLike.setTimeout(run, timeoutMs);
  return {
    cancel: () => {
      if (done) return;
      done = true;
      if (idleId !== null) windowLike.cancelIdleCallback?.(idleId);
      if (timer !== null) windowLike.clearTimeout(timer);
    },
  };
}
