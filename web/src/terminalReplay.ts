import { groundStateReplayStart } from "./terminalCache";

/**
 * The maximum amount of cached terminal history that one pane replays before
 * it attaches to the live stream. This is a per-pane main-thread budget: a
 * cache may retain more history, but waking eight panes cannot parse eight
 * complete multi-megabyte caches at once.
 */
export const REPLAY_TAIL_MAX_BYTES = 1 * 1024 * 1024;

/** Keep each xterm parser turn bounded and aligned with the server frame size. */
export const REPLAY_SLICE_BYTES = 64 * 1024;

export interface ReplayTail {
  /** The ground-state-safe tail to send to xterm. */
  data: Uint8Array;
  /** The stream position remains the end position after trimming the tail. */
  outputPosition: number;
  droppedBytes: number;
  droppedLines: number;
}

/** Count line feeds without decoding terminal output as text. */
export function lineCount(data: Uint8Array): number {
  let count = 0;
  for (const byte of data) if (byte === 0x0a) count += 1;
  return count;
}

/**
 * Return a tail no larger than `maxBytes` where possible. A cut is made only
 * just after a line feed: that is the ground-state seam used by #366 and does
 * not split an ANSI sequence or a UTF-8 code point. A newline-free record is
 * left intact because there is no safe client-side parser boundary in it.
 *
 * This returns a view, not a copy. The source is already owned by the replay
 * operation, so keeping the allocation out of the hot path is intentional.
 */
export function sliceForReplay(
  data: Uint8Array,
  maxBytes: number = REPLAY_TAIL_MAX_BYTES,
): Uint8Array {
  if (data.byteLength <= maxBytes || maxBytes <= 0) return data;
  const target = data.byteLength - maxBytes;
  const newline = data.indexOf(0x0a, target);
  return newline === -1 ? data : data.subarray(newline + 1);
}

/** Prepare a cached tail while preserving the absolute stream position. */
export function prepareReplayTail(
  data: Uint8Array,
  outputPosition: number,
  maxBytes: number = REPLAY_TAIL_MAX_BYTES,
): ReplayTail {
  // loadTerminalCache normally performs this trim already. Keeping the seam
  // here as well makes callers safe if they supply a raw cache entry or if the
  // cache format changes later.
  const groundStart = groundStateReplayStart(data, outputPosition);
  const grounded = groundStart === 0 ? data : data.subarray(groundStart);
  const replay = sliceForReplay(grounded, maxBytes);
  return {
    data: replay,
    outputPosition,
    droppedBytes: data.byteLength - replay.byteLength,
    droppedLines: lineCount(data) - lineCount(replay),
  };
}

/** Split replay data without copying it. */
export function replaySlices(
  data: Uint8Array,
  sliceBytes: number = REPLAY_SLICE_BYTES,
): Uint8Array[] {
  if (data.byteLength === 0) return [];
  const size = Math.max(1, sliceBytes);
  const slices: Uint8Array[] = [];
  for (let offset = 0; offset < data.byteLength; offset += size) {
    slices.push(data.subarray(offset, Math.min(offset + size, data.byteLength)));
  }
  return slices;
}

export interface ReplayMetrics {
  sessionId: string;
  inputBytes: number;
  writtenBytes: number;
  chunks: number;
  writtenChunks: number;
  deferredBytes: number;
  deferredLines: number;
  droppedBytes: number;
  droppedLines: number;
  durationMs: number;
  cancelled: boolean;
}

export interface ReplayOptions {
  kind: "cache" | "snapshot";
  droppedBytes?: number;
  droppedLines?: number;
  /** Optional test/consumer hook; instrumentation still happens first. */
  onComplete?: (metrics: ReplayMetrics) => void;
}

export type ReplayWrite = (chunk: Uint8Array, done: () => void) => void;

export interface ReplayHandle {
  /** Enqueue more data; useful while a WebSocket snapshot is arriving. */
  enqueue: (data: Uint8Array) => boolean;
  /** Mark the input complete. The handle resolves after xterm drains it. */
  finish: () => void;
  cancel: () => void;
  done: Promise<ReplayMetrics>;
}

interface ReplayTask {
  readonly sessionId: string;
  readonly write: ReplayWrite;
  readonly options: ReplayOptions;
  readonly metrics: ReplayMetrics;
  readonly pending: Uint8Array[];
  readonly startedAt: number;
  resolve: (metrics: ReplayMetrics) => void;
  finished: boolean;
  cancelled: boolean;
  inFlight: boolean;
  queued: boolean;
  complete: boolean;
}

// One global queue makes simultaneous pane restores take turns. A timer is
// used between turns so the browser gets a chance to paint and process input.
const ready: ReplayTask[] = [];
let pumpTimer: ReturnType<typeof setTimeout> | null = null;
let measurementId = 0;

function now(): number {
  return globalThis.performance?.now() ?? Date.now();
}

function enqueueReady(task: ReplayTask): void {
  if (task.cancelled || task.complete || task.queued || task.inFlight) return;
  if (task.pending.length === 0) return;
  task.queued = true;
  ready.push(task);
  requestPump();
}

function requestPump(): void {
  if (pumpTimer !== null) return;
  pumpTimer = setTimeout(() => {
    pumpTimer = null;
    pumpOne();
  }, 0);
}

function report(task: ReplayTask): void {
  task.metrics.durationMs = Math.max(0, now() - task.startedAt);
  const id = measurementId++;
  const startMark = `vogt-terminal-replay-${id}-start`;
  const endMark = `vogt-terminal-replay-${id}-end`;
  const measureName = `vogt-terminal-replay.${task.options.kind}`;
  const perf = globalThis.performance;
  if (perf?.mark && perf.measure) {
    perf.mark(startMark, { startTime: task.startedAt });
    perf.mark(endMark);
    perf.measure(measureName, startMark, endMark);
    perf.clearMarks(startMark);
    perf.clearMarks(endMark);
  }
  // Debug-level, structured enough for the performance harness to collect but
  // quiet enough not to become normal terminal UI output.
  console.debug("[vogt] terminal replay", {
    sessionId: task.sessionId,
    kind: task.options.kind,
    snapshotBytes: task.metrics.inputBytes,
    replayDurationMs: task.metrics.durationMs,
    chunks: task.metrics.writtenChunks,
    deferredBytes: task.metrics.deferredBytes,
    deferredLines: task.metrics.deferredLines,
    droppedBytes: task.metrics.droppedBytes,
    droppedLines: task.metrics.droppedLines,
    cancelled: task.metrics.cancelled,
  });
  task.options.onComplete?.(task.metrics);
}

function completeTask(task: ReplayTask): void {
  if (task.complete) return;
  task.complete = true;
  report(task);
  task.resolve(task.metrics);
}

function maybeComplete(task: ReplayTask): void {
  if (
    task.finished &&
    !task.inFlight &&
    task.pending.length === 0
  ) {
    completeTask(task);
  }
}

function pumpOne(): void {
  const task = ready.shift();
  if (!task) return;
  task.queued = false;
  if (task.cancelled || task.complete) {
    maybeComplete(task);
    requestPump();
    return;
  }
  const chunk = task.pending.shift();
  if (!chunk) {
    maybeComplete(task);
    requestPump();
    return;
  }
  task.inFlight = true;
  task.metrics.writtenChunks += 1;
  task.metrics.writtenBytes += chunk.byteLength;
  let callbackCalled = false;
  const done = () => {
    if (callbackCalled) return;
    callbackCalled = true;
    task.inFlight = false;
    if (task.pending.length > 0) enqueueReady(task);
    else maybeComplete(task);
    if (ready.length > 0) requestPump();
  };
  try {
    task.write(chunk, done);
  } catch {
    // A pane can unmount while a scheduled slice is being dispatched. Treat
    // that as a completed slice so it cannot strand every other pane.
    done();
  }
}

function makeTask(
  sessionId: string,
  write: ReplayWrite,
  options: ReplayOptions,
): ReplayTask & ReplayHandle {
  let resolveDone: (metrics: ReplayMetrics) => void = () => {};
  const task: ReplayTask = {
    sessionId,
    write,
    options,
    metrics: {
      sessionId,
      inputBytes: 0,
      writtenBytes: 0,
      chunks: 0,
      writtenChunks: 0,
      deferredBytes: 0,
      deferredLines: 0,
      droppedBytes: options.droppedBytes ?? 0,
      droppedLines: options.droppedLines ?? 0,
      durationMs: 0,
      cancelled: false,
    },
    pending: [],
    startedAt: now(),
    resolve: (metrics) => resolveDone(metrics),
    finished: false,
    cancelled: false,
    inFlight: false,
    queued: false,
    complete: false,
  };
  const done = new Promise<ReplayMetrics>((resolve) => {
    resolveDone = resolve;
  });
  const handle: ReplayHandle = {
    enqueue(data) {
      if (task.cancelled || task.complete || data.byteLength === 0) return false;
      for (const chunk of replaySlices(data)) {
        task.metrics.inputBytes += chunk.byteLength;
        task.metrics.chunks += 1;
        if (task.metrics.chunks > 1) {
          task.metrics.deferredBytes += chunk.byteLength;
          task.metrics.deferredLines += lineCount(chunk);
        }
        task.pending.push(chunk);
      }
      enqueueReady(task);
      return true;
    },
    finish() {
      if (task.cancelled || task.complete) return;
      task.finished = true;
      maybeComplete(task);
      enqueueReady(task);
    },
    cancel() {
      if (task.complete) return;
      task.cancelled = true;
      task.metrics.cancelled = true;
      task.finished = true;
      task.pending.length = 0;
      maybeComplete(task);
    },
    done,
  };
  return Object.assign(task, handle);
}

/** Schedule a complete replay, or use the returned handle for streaming input. */
export function scheduleReplay(
  sessionId: string,
  chunks: readonly Uint8Array[],
  write: ReplayWrite,
  options: ReplayOptions = { kind: "snapshot" },
): ReplayHandle {
  const task = makeTask(sessionId, write, options);
  for (const chunk of chunks) task.enqueue(chunk);
  task.finish();
  return task;
}

/** Create a replay queue for a snapshot whose frames arrive over WebSocket. */
export function createReplayQueue(
  sessionId: string,
  write: ReplayWrite,
  options: ReplayOptions,
): ReplayHandle {
  return makeTask(sessionId, write, options);
}
