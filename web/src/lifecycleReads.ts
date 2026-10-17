/** Cancellation for foreground reads that have been superseded (#411). */

const superseded = () =>
  new DOMException("A newer foreground read superseded this request.", "AbortError");

export interface ReadLease {
  signal: AbortSignal;
  generation: number;
  /** Mark this lease settled and detach any upstream listener. */
  finish: () => void;
}

export interface ReadCoordinator {
  /** Abort the previous read and start a new generation. */
  begin: (upstream?: AbortSignal) => ReadLease;
  /** Abort the current read, for example when a tab becomes hidden. */
  cancel: () => void;
  /** Cancel and release all coordinator state during component cleanup. */
  dispose: () => void;
}

/**
 * One in-flight foreground read per owner.  The generation is still useful
 * when a test double (or a non-fetch adapter) resolves after it was aborted:
 * callers can refuse to apply that stale answer as well as cancelling the
 * real network request.
 */
export function createReadCoordinator(): ReadCoordinator {
  let current: { controller: AbortController; finish: () => void } | null = null;
  let generation = 0;

  const begin = (upstream?: AbortSignal): ReadLease => {
    current?.controller.abort(superseded());
    current?.finish();

    const controller = new AbortController();
    const ownGeneration = ++generation;
    const onUpstreamAbort = () => {
      controller.abort(
        upstream?.reason ?? new DOMException("The read was cancelled.", "AbortError"),
      );
    };
    if (upstream) {
      if (upstream.aborted) onUpstreamAbort();
      else upstream.addEventListener("abort", onUpstreamAbort, { once: true });
    }
    let finished = false;
    const finish = () => {
      if (finished) return;
      finished = true;
      upstream?.removeEventListener("abort", onUpstreamAbort);
      if (current?.controller === controller) current = null;
    };
    current = { controller, finish };
    return { signal: controller.signal, generation: ownGeneration, finish };
  };

  const cancel = () => {
    current?.controller.abort(new DOMException("The read was cancelled.", "AbortError"));
  };

  const dispose = () => {
    cancel();
    current?.finish();
    current = null;
  };

  return { begin, cancel, dispose };
}
