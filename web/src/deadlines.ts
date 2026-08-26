/** Foreground read deadlines, by operation class. */
export type OpClass = "auth" | "metadata" | "list" | "detail" | "long";

/**
 * Generous enough for a healthy remote deployment, while preventing a
 * suspended browser connection from blocking boot or reconciliation forever.
 */
export const DEADLINE_MS: Record<OpClass, number> = {
  auth: 4_000,
  metadata: 4_000,
  list: 8_000,
  detail: 8_000,
  long: 30_000,
};

export interface DeadlineSignal {
  signal: AbortSignal;
  cancel: () => void;
}

/** Compose upstream cancellation with a controllable timeout signal. */
export function deadline(cls: OpClass, upstream?: AbortSignal): DeadlineSignal {
  const controller = new AbortController();
  let settled = false;
  let timer: ReturnType<typeof setTimeout> | undefined;

  const cleanup = () => {
    if (timer !== undefined) clearTimeout(timer);
    upstream?.removeEventListener("abort", onAbort);
  };
  const abort = (reason: unknown) => {
    if (settled) return;
    settled = true;
    cleanup();
    controller.abort(reason);
  };
  const onAbort = () =>
    abort(upstream?.reason ?? new DOMException("The operation was aborted.", "AbortError"));

  timer = setTimeout(
    () =>
      abort(
        new DOMException(
          `${cls} read exceeded its ${DEADLINE_MS[cls]}ms deadline`,
          "TimeoutError",
        ),
      ),
    DEADLINE_MS[cls],
  );
  if (upstream?.aborted) onAbort();
  else upstream?.addEventListener("abort", onAbort, { once: true });

  return {
    signal: controller.signal,
    cancel: () => abort(new DOMException("The operation was cancelled.", "AbortError")),
  };
}
