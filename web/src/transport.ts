// Shared retry policy above the runtime transport selected at application boot.
// (engine calls) and `call()` in `vogtApi.ts` (Vogt calls). It exists because a
// dropped connection used to reach a surface as a raw `TypeError`
// ("NetworkError when attempting to fetch resource"), rendered verbatim as the
// view's own failure with no way to recover (#198).
//
// `fetch` rejects ONLY on a transport-level failure (the stream was reset
// before any response arrived) or a deliberate abort. An HTTP error — 4xx, 5xx,
// the 502/503 the front door raises as `VogtUnavailable` — resolves with
// `res.ok === false`, so it never reaches the `catch` here and keeps its
// existing, meaningful handling. This layer therefore classifies exactly the
// two things `fetch` rejects with:
//
//   * a deliberate abort (Stop button, SSE teardown, palette cancellation) —
//     re-thrown untouched so those paths keep working;
//   * a transient wire failure — retried for idempotent methods, then raised as
//     a typed `TransportError` carrying a written reason instead of the
//     browser's implementation detail.

import { runtimeTransport } from "./runtimeTransport";

/** A transport-level failure: the request never reached a responding server. */
export class TransportError extends Error {
  constructor(public readonly cause?: unknown) {
    super(
      "The connection was interrupted before the server could answer. " +
        "Check your network and try again.",
    );
    this.name = "TransportError";
  }
}

/** Only idempotent methods are safe to retry: a blind POST retry could
 *  double-write (`work.create`, `work.transition` share the Vogt `call()`). */
const RETRYABLE_METHODS = new Set(["GET", "HEAD"]);

/** True when a rejection is a deliberate cancellation rather than a wire
 *  failure — either an already-aborted signal or an `AbortError`. */
function isAbort(err: unknown, signal?: AbortSignal | null): boolean {
  if (signal?.aborted) return true;
  return (err as { name?: string } | null)?.name === "AbortError";
}

function jitter(baseMs: number): number {
  // Full jitter over [base, 2*base): spreads reconnection attempts so a whole
  // tab's worth of panels do not retry in lockstep after a blip.
  return baseMs + Math.random() * baseMs;
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export interface RetryOptions {
  /** Extra attempts after the first, for GET/HEAD only. Default 2. */
  retries?: number;
  /** Base backoff before the first retry, growing linearly. Default 150ms. */
  backoffMs?: number;
  /** Abort an attempt after this many milliseconds. */
  deadlineMs?: number;
}

/**
 * `fetch` with transport-failure classification and idempotent-only retry.
 *
 * Resolves with the `Response` (whatever its status) exactly as `fetch` would,
 * so all HTTP-status handling stays with the caller. Rejects with the original
 * `AbortError` on a deliberate abort, or a `TransportError` once a transient
 * wire failure has exhausted its retries.
 */
export async function fetchWithRetry(
  url: string,
  init: RequestInit = {},
  opts: RetryOptions = {},
): Promise<Response> {
  const method = (init.method ?? "GET").toUpperCase();
  const attempts = RETRYABLE_METHODS.has(method) ? (opts.retries ?? 2) + 1 : 1;
  const backoffMs = opts.backoffMs ?? 150;
  const signal = init.signal ?? undefined;

  let lastError: unknown;
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    const controller = opts.deadlineMs === undefined ? null : new AbortController();
    const deadline = controller === null ? null : setTimeout(() => controller.abort(), opts.deadlineMs);
    const requestController = controller;
    const cancel = requestController !== null && signal !== undefined
      ? () => requestController.abort()
      : null;
    if (requestController !== null && signal !== undefined && cancel !== null) {
      if (signal.aborted) requestController.abort();
      else signal.addEventListener("abort", cancel, { once: true });
    }
    const requestInit = controller === null
      ? init
      : { ...init, signal: controller.signal };
    try {
      return await runtimeTransport().request(url, requestInit);
    } catch (err) {
      if (isAbort(err, signal)) throw err;
      lastError = err;
      const isLast = attempt === attempts - 1;
      if (isLast) break;
      await sleep(jitter(backoffMs * (attempt + 1)));
      // The caller may have given up during the backoff; honour it rather than
      // firing another request into a signal that is now aborted.
      if (signal?.aborted) throw new DOMException("Aborted", "AbortError");
    } finally {
      if (deadline !== null) clearTimeout(deadline);
      if (requestController !== null && signal !== undefined && cancel !== null) {
        signal.removeEventListener("abort", cancel);
      }
    }
  }
  throw new TransportError(lastError);
}
