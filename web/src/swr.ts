/** A small in-memory TTL/stale-while-revalidate cache for read-only metadata.
 *
 * The cache deliberately has no persistence layer and no write API. Callers
 * opt into it at a read boundary, so a cached value cannot become an input to
 * a mutation by accident. The identity is part of every key: changing the
 * backend or credential can never read another identity's entry, even if a
 * caller forgets to invalidate first.
 */

export interface CachePolicy {
  /** How long a value may be returned without starting a request. */
  ttlMs: number;
  /** How long a stale value may be shown while one refresh runs in the background. */
  swrMs: number;
}

export interface CacheIdentity {
  base: string;
  token: string;
  /** Optional tenant/project scope for callers whose URL does not carry it. */
  scope?: string;
}

export const STABLE_READ_POLICY: CachePolicy = Object.freeze({
  ttlMs: 30_000,
  swrMs: 120_000,
});

export const PUBLIC_CONFIG_POLICY: CachePolicy = Object.freeze({
  ttlMs: 300_000,
  swrMs: 900_000,
});

interface CacheEntry<T> {
  value: T;
  etag: string | null;
  fetchedAt: number;
  stamp: CacheStamp;
  backgroundFailures: number;
  nextRetryAt: number | null;
}

export interface CacheFetchResult<T> {
  value: T;
  etag: string | null;
}

export type ConditionalCacheLoader<T> = (
  etag: string | null,
  signal?: AbortSignal,
) => Promise<CacheFetchResult<T> | "not-modified">;

interface CacheStamp {
  all: number;
  key: number;
}

const KEY_PREFIX = "vogt-client:";
const entries = new Map<string, CacheEntry<unknown>>();
const inFlight = new Map<string, Promise<unknown>>();
const keyGenerations = new Map<string, number>();
let allGeneration = 0;
const BACKGROUND_RETRY_DELAY_MS = 1_000;
const MAX_BACKGROUND_FAILURES = 2;

function fingerprint(value: string): string {
  // The cache is process-local, but credentials still should not appear in
  // cache keys that a debugger or diagnostic can inspect. Two independent
  // 32-bit FNV passes give a deterministic opaque partition without making
  // cache-key construction asynchronous like SubtleCrypto is in a browser.
  let first = 0x811c9dc5;
  let second = 0x811c9dc5 ^ 0x9e3779b9;
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    first = Math.imul(first ^ code, 0x01000193);
    second = Math.imul(second ^ (code + index), 0x01000193);
  }
  return `${(first >>> 0).toString(16)}${(second >>> 0).toString(16)}`;
}

function stableSerialize(value: unknown): string {
  if (value === null || typeof value !== "object") {
    return JSON.stringify(value) ?? "undefined";
  }
  if (Array.isArray(value)) {
    return `[${value.map((item) => stableSerialize(item)).join(",")}]`;
  }
  const object = value as Record<string, unknown>;
  return `{${Object.keys(object)
    .sort()
    .map((key) => `${JSON.stringify(key)}:${stableSerialize(object[key])}`)
    .join(",")}}`;
}

function stampFor(key: string): CacheStamp {
  return { all: allGeneration, key: keyGenerations.get(key) ?? 0 };
}

function sameStamp(left: CacheStamp, right: CacheStamp): boolean {
  return left.all === right.all && left.key === right.key;
}

function operationPrefix(operation: string): string {
  return `${KEY_PREFIX}${operation}\u0000`;
}

export function cacheIdentity(
  base: string,
  token: string,
  scope?: string,
): CacheIdentity {
  return {
    base: base.replace(/\/+$/, ""),
    token: fingerprint(token),
    ...(scope === undefined ? {} : { scope }),
  };
}

/** Build an identity- and parameter-isolated key for one read operation. */
export function cacheKey(
  identity: CacheIdentity,
  operation: string,
  params: unknown = {},
): string {
  return `${KEY_PREFIX}${operation}\u0000${stableSerialize({ identity, params })}`;
}

async function load<T>(
  key: string,
  loader: ConditionalCacheLoader<T>,
  signal: AbortSignal | undefined,
): Promise<T> {
  const existing = inFlight.get(key);
  if (existing) return existing as Promise<T>;

  const stamp = stampFor(key);
  const previous = entries.get(key) as CacheEntry<T> | undefined;
  const request = Promise.resolve()
    .then(() => loader(previous?.etag ?? null, signal))
    .then((result) => {
      const value = result === "not-modified" ? previous?.value : result.value;
      if (value === undefined) {
        throw new Error("conditional response has no cached value");
      }
      if (sameStamp(stamp, stampFor(key))) {
        entries.set(key, {
          value,
          etag: result === "not-modified" ? previous?.etag ?? null : result.etag,
          fetchedAt: Date.now(),
          stamp,
          backgroundFailures: 0,
          nextRetryAt: null,
        });
      }
      return value;
    });
  inFlight.set(key, request);
  // Cleanup is a separate, always-resolved observer. Keeping the original
  // request as the tracked value means foreground callers still receive the
  // loader's error, while a background caller can consume it explicitly
  // without producing a second unhandled rejection chain.
  void request.then(
    () => {
      if (inFlight.get(key) === request) inFlight.delete(key);
    },
    () => {
      if (inFlight.get(key) === request) inFlight.delete(key);
    },
  );
  return request;
}

/**
 * Read through the cache.
 *
 * Fresh entries resolve without calling `loader`. Stale entries resolve
 * immediately and share one background refresh. Once the bounded stale
 * window ends, the refresh must succeed before a value is returned.
 */
export async function cachedRead<T>(
  key: string,
  loader: (signal?: AbortSignal) => Promise<T>,
  policy: CachePolicy,
  signal?: AbortSignal,
): Promise<T> {
  return cachedReadWithLoader(
    key,
    (_etag, requestSignal) =>
      loader(requestSignal).then((value) => ({ value, etag: null })),
    policy,
    signal,
  );
}

/** Read through the cache while retaining and revalidating an HTTP validator. */
export async function cachedReadWithValidator<T>(
  key: string,
  loader: ConditionalCacheLoader<T>,
  policy: CachePolicy,
  signal?: AbortSignal,
): Promise<T> {
  return cachedReadWithLoader(key, loader, policy, signal);
}

async function cachedReadWithLoader<T>(
  key: string,
  loader: ConditionalCacheLoader<T>,
  policy: CachePolicy,
  signal?: AbortSignal,
): Promise<T> {
  if (signal?.aborted) {
    throw signal.reason ?? new DOMException("The operation was aborted.", "AbortError");
  }
  const entry = entries.get(key) as CacheEntry<T> | undefined;
  if (!entry) return load(key, loader, signal);

  const age = Math.max(0, Date.now() - entry.fetchedAt);
  if (age < policy.ttlMs) return entry.value;

  if (age < policy.ttlMs + policy.swrMs) {
    const now = Date.now();
    const retryAllowed =
      entry.backgroundFailures < MAX_BACKGROUND_FAILURES &&
      (entry.nextRetryAt === null || now >= entry.nextRetryAt);
    if (retryAllowed) {
      entry.nextRetryAt = null;
      void load(key, loader, undefined).catch(() => {
        const current = entries.get(key);
        if (current !== entry) return;
        current.backgroundFailures += 1;
        current.nextRetryAt =
          current.backgroundFailures < MAX_BACKGROUND_FAILURES
            ? Date.now() + BACKGROUND_RETRY_DELAY_MS
            : null;
      });
    }
    return entry.value;
  }

  return load(key, loader, signal);
}

/** Invalidate one operation family, or all cached read data when omitted. */
export function invalidate(operation?: string): void {
  if (operation === undefined) {
    allGeneration += 1;
    entries.clear();
    inFlight.clear();
    keyGenerations.clear();
    return;
  }

  const prefix = operationPrefix(operation);
  for (const key of entries.keys()) {
    if (key.startsWith(prefix)) {
      entries.delete(key);
      keyGenerations.set(key, (keyGenerations.get(key) ?? 0) + 1);
    }
  }
  for (const key of inFlight.keys()) {
    if (key.startsWith(prefix)) {
      inFlight.delete(key);
      keyGenerations.set(key, (keyGenerations.get(key) ?? 0) + 1);
    }
  }
}
