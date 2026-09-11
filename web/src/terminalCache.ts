const DB_NAME = "vogt-terminal-cache";
const STORE_NAME = "sessions";
const DB_VERSION = 1;
const MAX_CACHED_SESSIONS = 8;

export const MAX_TERMINAL_CACHE_BYTES = 4 * 1024 * 1024;

/**
 * A cached terminal, in one of two shapes (F5, WI-129):
 *
 * - `serialized`: a live pane persists its xterm screen + capped scrollback via
 *   `@xterm/addon-serialize`. On reload it restores in a single `term.write`,
 *   with no raw-byte re-parse — the fast path.
 * - `data`: raw scrollback bytes. Written by the headless pre-warm path
 *   (`terminalPrewarm`), which has no xterm to serialize, and by a live pane as
 *   a fallback when serialization is unavailable. Restored by re-parsing a
 *   ground-state-aligned tail (the pre-F5 path), which also reads any entry a
 *   pre-F5 client left behind — so the format change needs no DB version bump
 *   or migration.
 *
 * Exactly one of `serialized` / `data` is present.
 */
export interface TerminalCacheEntry {
  sessionId: string;
  outputPosition: number;
  updatedAt: number;
  serialized?: string;
  data?: ArrayBuffer;
}

/** What a caller hands `saveTerminalCache`: a serialized screen or raw bytes. */
export type TerminalCachePayload =
  | { serialized: string }
  | { data: Uint8Array };

/**
 * The client cache is a byte-oriented ring (see `appendToCache` in
 * Terminal.tsx): once it overflows, its oldest bytes are dropped at an
 * arbitrary offset. Replaying such a tail into a freshly-reset xterm.js can
 * begin in the middle of an ANSI escape sequence or a UTF-8 multibyte
 * character — the client-side twin of the server's mid-sequence-cut bug, and the source of the
 * garbled top-of-viewport line (a chopped `\x1b[…m` eats the head of the next
 * line, a lone continuation byte renders as mojibake).
 *
 * Return the offset at which a cached tail is safe to replay from. When the
 * cache still holds the whole stream from byte 0 (`outputPosition` equals its
 * length) nothing was dropped and the start is already ground state, so we
 * return 0. Otherwise we advance to just past the first newline: a line feed
 * never sits inside a CSI/OSC sequence and `0x0A` is never a UTF-8
 * continuation byte, so the parser is guaranteed to be in its ground state
 * there. If the tail has no newline at all we leave it as-is.
 */
export function groundStateReplayStart(
  data: Uint8Array,
  outputPosition: number,
): number {
  if (outputPosition <= data.byteLength) return 0;
  const nl = data.indexOf(0x0a);
  return nl === -1 ? 0 : nl + 1;
}

function openCache(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, DB_VERSION);
    request.onupgradeneeded = () => {
      const db = request.result;
      if (!db.objectStoreNames.contains(STORE_NAME)) {
        const store = db.createObjectStore(STORE_NAME, { keyPath: "sessionId" });
        store.createIndex("updatedAt", "updatedAt");
      }
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

function requestResult<T>(request: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

export async function loadTerminalCache(
  sessionId: string,
): Promise<TerminalCacheEntry | null> {
  if (typeof indexedDB === "undefined") return null;
  try {
    const db = await openCache();
    const tx = db.transaction(STORE_NAME, "readonly");
    const result = await requestResult(
      tx.objectStore(STORE_NAME).get(sessionId) as IDBRequest<
        TerminalCacheEntry | undefined
      >,
    );
    db.close();
    if (!result || !Number.isSafeInteger(result.outputPosition)) return null;

    // Serialized fast path: a live pane's screen, restored in one write.
    if (typeof result.serialized === "string") {
      return result.serialized.length > 0 ? result : null;
    }
    // Raw path (pre-warm entries, live-pane fallback, pre-F5 entries). Detect
    // the ArrayBuffer realm-safely (`instanceof` misses a cross-realm buffer, as
    // a structured clone can produce).
    if (
      result.data != null &&
      Object.prototype.toString.call(result.data) === "[object ArrayBuffer]" &&
      result.outputPosition >= result.data.byteLength
    ) {
      // Drop any partial leading escape sequence / UTF-8 char left by the ring
      // trim so the tail replays from a terminal ground state.
      const bytes = new Uint8Array(result.data);
      const start = groundStateReplayStart(bytes, result.outputPosition);
      if (start > 0) {
        result.data = bytes.slice(start).buffer;
      }
      return result;
    }
    return null;
  } catch {
    return null;
  }
}

export async function saveTerminalCache(
  sessionId: string,
  outputPosition: number,
  payload: TerminalCachePayload,
): Promise<void> {
  if (typeof indexedDB === "undefined" || !Number.isSafeInteger(outputPosition)) {
    return;
  }
  try {
    const db = await openCache();
    const tx = db.transaction(STORE_NAME, "readwrite");
    const store = tx.objectStore(STORE_NAME);
    const stored: TerminalCacheEntry =
      "serialized" in payload
        ? { sessionId, outputPosition, serialized: payload.serialized, updatedAt: Date.now() }
        : { sessionId, outputPosition, data: payload.data.slice().buffer, updatedAt: Date.now() };
    store.put(stored);

    const entries = await requestResult(
      store.index("updatedAt").getAllKeys() as IDBRequest<IDBValidKey[]>,
    );
    for (const stale of entries.slice(0, -MAX_CACHED_SESSIONS)) {
      store.delete(stale);
    }
    await new Promise<void>((resolve, reject) => {
      tx.oncomplete = () => resolve();
      tx.onerror = () => reject(tx.error);
      tx.onabort = () => reject(tx.error);
    });
    db.close();
  } catch {
    /* Cache failure should never prevent terminal attachment. */
  }
}
