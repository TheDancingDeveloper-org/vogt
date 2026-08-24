const DB_NAME = "mydevenv2-terminal-cache";
const STORE_NAME = "sessions";
const DB_VERSION = 1;
const MAX_CACHED_SESSIONS = 8;

export const MAX_TERMINAL_CACHE_BYTES = 4 * 1024 * 1024;

export interface TerminalCacheEntry {
  sessionId: string;
  outputPosition: number;
  data: ArrayBuffer;
  updatedAt: number;
}

/**
 * The client cache is a byte-oriented ring (see `appendToCache` in
 * Terminal.tsx): once it overflows, its oldest bytes are dropped at an
 * arbitrary offset. Replaying such a tail into a freshly-reset xterm.js can
 * begin in the middle of an ANSI escape sequence or a UTF-8 multibyte
 * character — the client-side twin of issue #366, and the source of the
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
    if (
      !result ||
      !(result.data instanceof ArrayBuffer) ||
      !Number.isSafeInteger(result.outputPosition) ||
      result.outputPosition < result.data.byteLength
    ) {
      return null;
    }
    // Drop any partial leading escape sequence / UTF-8 char left by the ring
    // trim so the tail replays from a terminal ground state (issue #366).
    const bytes = new Uint8Array(result.data);
    const start = groundStateReplayStart(bytes, result.outputPosition);
    if (start > 0) {
      result.data = bytes.slice(start).buffer;
    }
    return result;
  } catch {
    return null;
  }
}

export async function saveTerminalCache(
  sessionId: string,
  outputPosition: number,
  data: Uint8Array,
): Promise<void> {
  if (typeof indexedDB === "undefined" || !Number.isSafeInteger(outputPosition)) {
    return;
  }
  try {
    const db = await openCache();
    const tx = db.transaction(STORE_NAME, "readwrite");
    const store = tx.objectStore(STORE_NAME);
    store.put({
      sessionId,
      outputPosition,
      data: data.slice().buffer,
      updatedAt: Date.now(),
    } satisfies TerminalCacheEntry);

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
