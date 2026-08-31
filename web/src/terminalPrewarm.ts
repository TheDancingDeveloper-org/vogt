// Background pre-warm of session panes.
//
// In a fresh browser every session takes the expensive cold-attach path the
// first time it is clicked, one at a time, while the reader waits. This module
// warms the most-recently-active sessions in the background so a later click is
// cheap: it opens a bounded cold attach that requests only the snapshot tail
// (the #474 cap, sent by `openAttach` when no `resume_from` is present), writes
// the tail and its absolute `outputPosition` to the same IndexedDB cache the
// restore path reads, then fully disconnects. When the reader later opens that
// session, `Terminal` restores from the cache and reattaches with a cheap
// `resume_from` delta (`reset:false`) instead of a cold full snapshot.
//
// Design decisions (all from issue #476):
//   1. Warm up to `MAX_PREWARM_SESSIONS` (== `MAX_CACHED_SESSIONS`), most
//      recently active first, each a single bounded warm attach.
//   2. Foreground preempts background. We deliberately do NOT touch the shared
//      FIFO replay queue in `terminalReplay.ts`: a warm attach never writes to
//      xterm, so it can never sit in that queue ahead of the pane the reader
//      clicked. On top of that structural guarantee we PAUSE the warm-up loop
//      while any foreground pane is performing its initial replay/attach
//      (`foregroundReplayActive()`), which is the simpler of the two options
//      the issue offers.
//   3. Concurrency is 1 (strictly sequential). We never wake many sockets at
//      once (#466 / the outbound coalesce cap), and the gate is re-checked
//      between every attach.
//   4. Document visibility is respected: the loop pauses while hidden and is
//      kicked again when the tab becomes visible.
//   5. Warm-up does not begin until after boot AND the active pane (if any) has
//      finished its own replay — the active pane holds `foregroundReplayActive`
//      until its snapshot completes, which keeps the loop paused until then.
//   6. Only sessions the reader has NOT already opened (which already have a
//      pane and cache) are warmed.
//
// This is a WebSocket-only worker: it never mounts an xterm and never runs the
// `Terminal` park/connect state machine, so it carries no risk to the #466
// no-socket-leak invariant. Every warm attach owns exactly one socket and
// always closes it (success, failure, timeout, or abort).

import type { SessionSummary } from "./api";
import { openAttach } from "./api";
import { MAX_TERMINAL_CACHE_BYTES, saveTerminalCache } from "./terminalCache";
import { sessionsStore } from "./store";
import { tabsStore } from "./tabs";

/**
 * How many sessions to warm. Matches `MAX_CACHED_SESSIONS` in
 * `terminalCache.ts` and the eight-pane replay ceiling in `terminalReplay.ts`:
 * warming more than the cache can retain would only evict what we just warmed.
 */
export const MAX_PREWARM_SESSIONS = 8;

/** A warm attach that never completes is abandoned (and its socket closed). */
const WARM_ATTACH_TIMEOUT_MS = 15_000;

/** Politeness delay before the first warm attach after `start()`. */
const START_DELAY_MS = 750;

// ---------------------------------------------------------------------------
// Foreground-replay gate. Shared module state so `Terminal.tsx` can pause the
// default coordinator while a real pane is attaching, and the coordinator can
// read it. A counter (not a boolean) so overlapping foreground panes compose.
// ---------------------------------------------------------------------------

let foregroundReplays = 0;
const foregroundIdleListeners = new Set<() => void>();

/** True while at least one foreground pane is performing its initial replay. */
export function foregroundReplayActive(): boolean {
  return foregroundReplays > 0;
}

/**
 * Mark a foreground pane's initial replay/attach as in progress. Returns an
 * idempotent disposer; call it once the pane's snapshot has finished, when the
 * pane parks, or on unmount. Warm-up is paused for the whole window.
 */
export function beginForegroundReplay(): () => void {
  foregroundReplays += 1;
  let released = false;
  return () => {
    if (released) return;
    released = true;
    foregroundReplays = Math.max(0, foregroundReplays - 1);
    if (foregroundReplays === 0) {
      for (const listener of foregroundIdleListeners) listener();
    }
  };
}

/** For tests: forget any leaked foreground tokens between cases. */
export function resetForegroundReplayForTest(): void {
  foregroundReplays = 0;
}

// ---------------------------------------------------------------------------
// Target selection (pure).
// ---------------------------------------------------------------------------

/**
 * The sessions to warm, most-recently-active first, excluding those the reader
 * already opened and those already warmed, capped at `limit`. A session that
 * has exited is skipped: attaching to a dead PTY yields nothing worth caching.
 */
export function selectPrewarmTargets(
  sessions: readonly SessionSummary[],
  openSessionIds: ReadonlySet<string>,
  alreadyWarmed: ReadonlySet<string>,
  limit: number = MAX_PREWARM_SESSIONS,
): string[] {
  const recency = (s: SessionSummary): number =>
    Date.parse(s.activity_changed_at || s.created_at) || 0;
  return sessions
    .filter(
      (s) =>
        s.exit_code === null &&
        !openSessionIds.has(s.id) &&
        !alreadyWarmed.has(s.id),
    )
    .sort((a, b) => recency(b) - recency(a))
    .slice(0, Math.max(0, limit))
    .map((s) => s.id);
}

// ---------------------------------------------------------------------------
// A single warm attach (WebSocket only, no xterm).
// ---------------------------------------------------------------------------

function concatChunks(chunks: readonly Uint8Array[], total: number): Uint8Array {
  const out = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return out;
}

/**
 * Open one cold attach, capture the snapshot tail and its end position, write
 * them to the terminal cache, then close the socket. Resolves `true` when a
 * usable tail was cached, `false` otherwise. Never rejects, and always leaves
 * the socket closed.
 */
export function warmAttachOnce(
  sessionId: string,
  signal?: AbortSignal,
): Promise<boolean> {
  return new Promise<boolean>((resolve) => {
    // Cold attach: no `resume_from`, so `openAttach` sends the #474 tail cap
    // and the server trims the full snapshot to it.
    const ws = openAttach(sessionId);
    ws.binaryType = "arraybuffer";

    let settled = false;
    let inSnapshot = false;
    let endPosition = 0;
    let chunks: Uint8Array[] = [];
    let bytes = 0;
    let timer: ReturnType<typeof setTimeout> | null = setTimeout(
      () => finish(false),
      WARM_ATTACH_TIMEOUT_MS,
    );

    const finish = (ok: boolean) => {
      if (settled) return;
      settled = true;
      if (timer !== null) {
        clearTimeout(timer);
        timer = null;
      }
      signal?.removeEventListener("abort", onAbort);
      // Always drop the socket — a warm attach owns exactly one socket and
      // never leaves a second live socket on a session (#466 invariant).
      try {
        ws.close(1000, "prewarm complete");
      } catch {
        /* already closing */
      }
      resolve(ok);
    };

    const onAbort = () => finish(false);
    signal?.addEventListener("abort", onAbort);
    if (signal?.aborted) {
      finish(false);
      return;
    }

    ws.addEventListener("message", (ev) => {
      if (settled) return;
      if (typeof ev.data === "string") {
        try {
          const ctrl = JSON.parse(ev.data) as {
            type: string;
            scrollback_pos?: number;
            scrollback_bytes?: number;
          };
          if (ctrl.type === "snapshot-start") {
            inSnapshot = true;
            chunks = [];
            bytes = 0;
            endPosition =
              typeof ctrl.scrollback_pos === "number" ? ctrl.scrollback_pos : 0;
          } else if (ctrl.type === "snapshot-done") {
            // Stop appending before the async save so trailing live frames
            // cannot desynchronise the tail from `endPosition`.
            inSnapshot = false;
            if (bytes === 0) {
              // Nothing to restore from; caching an empty tail would only evict
              // a real entry (`saveTerminalCache` keeps the newest eight).
              finish(false);
              return;
            }
            const data = concatChunks(chunks, bytes);
            void saveTerminalCache(sessionId, endPosition, data).finally(() =>
              finish(true),
            );
          }
        } catch {
          /* ignore non-JSON control frames */
        }
        return;
      }
      if (!inSnapshot) return;
      const buf = new Uint8Array(ev.data as ArrayBuffer);
      chunks.push(buf);
      bytes += buf.byteLength;
      // Defensive: the server already caps the cold snapshot to the tail
      // budget, but keep the client buffer bounded regardless.
      while (bytes > MAX_TERMINAL_CACHE_BYTES && chunks.length > 1) {
        const first = chunks[0];
        if (!first) break;
        chunks.shift();
        bytes -= first.byteLength;
      }
    });

    ws.addEventListener("close", () => finish(false));
    ws.addEventListener("error", () => finish(false));
  });
}

// ---------------------------------------------------------------------------
// The coordinator.
// ---------------------------------------------------------------------------

export interface PrewarmDeps {
  /** The live session list to choose targets from. */
  listSessions: () => readonly SessionSummary[];
  /** Session ids the reader already opened (they own a pane and cache). */
  openSessionIds: () => ReadonlySet<string>;
  /** Whether the document is currently visible. */
  isVisible: () => boolean;
  /** Whether a foreground pane is mid initial replay/attach. */
  foregroundActive: () => boolean;
  /** Perform one warm attach for a session. */
  warmAttach: (sessionId: string, signal?: AbortSignal) => Promise<boolean>;
  /** Max sessions to warm. */
  limit?: number;
  /** Delay before the first attach after `start()`. */
  startDelayMs?: number;
}

export interface PrewarmCoordinator {
  /** Begin (idempotent). Schedules the first pass after the start delay. */
  start: () => void;
  /** Stop and abort any in-flight attach. */
  stop: () => void;
  /** Re-evaluate now (e.g. tab became visible, or foreground went idle). */
  kick: () => void;
  /** Session ids warmed so far (for tests). */
  warmed: () => ReadonlySet<string>;
  /** Await the current pass, if one is running (for tests). */
  idle: () => Promise<void>;
}

export function createPrewarmCoordinator(
  deps: PrewarmDeps,
): PrewarmCoordinator {
  const limit = deps.limit ?? MAX_PREWARM_SESSIONS;
  const startDelayMs = deps.startDelayMs ?? START_DELAY_MS;
  const warmed = new Set<string>();

  let started = false;
  let stopped = false;
  let running: Promise<void> | null = null;
  let startTimer: ReturnType<typeof setTimeout> | null = null;
  let abort: AbortController | null = null;

  const paused = (): boolean =>
    stopped || !deps.isVisible() || deps.foregroundActive();

  async function runPass(): Promise<void> {
    // Strictly sequential: one socket at a time, re-checking the gate between
    // every attach so a foreground open pauses the very next warm attach.
    while (!paused()) {
      const targets = selectPrewarmTargets(
        deps.listSessions(),
        deps.openSessionIds(),
        warmed,
        limit,
      );
      const id = targets[0];
      if (id === undefined) break;
      // Mark before attaching so a concurrent selection cannot re-pick it and
      // so a failed attach is not retried in a tight loop.
      warmed.add(id);
      abort = new AbortController();
      try {
        await deps.warmAttach(id, abort.signal);
      } catch {
        /* a warm attach never rejects, but never let one stall the pass */
      } finally {
        abort = null;
      }
    }
  }

  function kick(): void {
    if (!started || stopped || running || paused()) return;
    running = runPass().finally(() => {
      running = null;
    });
  }

  function start(): void {
    if (started) return;
    started = true;
    stopped = false;
    if (startTimer !== null) clearTimeout(startTimer);
    startTimer = setTimeout(() => {
      startTimer = null;
      kick();
    }, startDelayMs);
  }

  function stop(): void {
    stopped = true;
    started = false;
    if (startTimer !== null) {
      clearTimeout(startTimer);
      startTimer = null;
    }
    abort?.abort();
  }

  return {
    start,
    stop,
    kick,
    warmed: () => warmed,
    idle: async () => {
      await running;
    },
  };
}

// ---------------------------------------------------------------------------
// The default singleton, wired to the real stores. Imported lazily inside the
// accessors so this module stays cheap to import from `Terminal.tsx` (which
// only needs the foreground gate) and unit-testable without the stores.
// ---------------------------------------------------------------------------

let singleton: PrewarmCoordinator | null = null;

function defaultCoordinator(): PrewarmCoordinator {
  if (singleton) return singleton;
  singleton = createPrewarmCoordinator({
    listSessions: () => Object.values(sessionsStore.sessions),
    openSessionIds: () =>
      new Set(
        tabsStore.tabs
          .filter((tab): tab is Extract<typeof tab, { kind: "terminal" }> =>
            tab.kind === "terminal",
          )
          .map((tab) => tab.sessionId),
      ),
    isVisible: () =>
      typeof document === "undefined" ||
      document.visibilityState === "visible",
    foregroundActive: foregroundReplayActive,
    warmAttach: warmAttachOnce,
  });
  // A foreground pane going idle, or the tab becoming visible, both re-open the
  // gate — re-evaluate then.
  foregroundIdleListeners.add(() => singleton?.kick());
  if (typeof document !== "undefined") {
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "visible") singleton?.kick();
    });
  }
  return singleton;
}

/** Begin background pre-warm. Safe to call more than once. */
export function startPrewarm(): void {
  defaultCoordinator().start();
}

/** Re-evaluate the default coordinator (session list changed, etc.). */
export function kickPrewarm(): void {
  defaultCoordinator().kick();
}
