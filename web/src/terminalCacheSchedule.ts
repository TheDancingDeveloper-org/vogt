/**
 * When the terminal cache is allowed to hit IndexedDB (WI-2695).
 *
 * The live ring still appends per frame. A persist is scheduled only when the
 * byte cursor moved, and only for the visible pane on the 5 s cadence. A hidden
 * pane persists once, on the way into dormancy, not on every later chunk.
 * The write itself is deferred to idle time so serialize + the IDB put are not
 * a long task on the frame that received the bytes.
 */

export const CACHE_PERSIST_INTERVAL_MS = 5_000;
export const CACHE_PERSIST_IDLE_TIMEOUT_MS = 2_000;

export function cacheNeedsPersist(
  outputPosition: number | undefined,
  lastPersistedPosition: number | undefined,
): outputPosition is number {
  return outputPosition !== undefined && outputPosition !== lastPersistedPosition;
}

/**
 * A hidden pane that has already been persisted at this cursor should not
 * re-schedule. The visible pane keeps the 5 s cadence so a reconnect still
 * sees recent scrollback.
 */
export function shouldScheduleCachePersist(input: {
  outputPosition: number | undefined;
  lastPersistedPosition: number | undefined;
  hidden: boolean;
  alreadyScheduled: boolean;
}): boolean {
  if (input.alreadyScheduled) return false;
  if (!cacheNeedsPersist(input.outputPosition, input.lastPersistedPosition)) return false;
  if (input.hidden && input.lastPersistedPosition !== undefined) return false;
  return true;
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
