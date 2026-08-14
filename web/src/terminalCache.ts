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
