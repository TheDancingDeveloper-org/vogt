import {
  createEffect,
  createSignal,
  onCleanup,
  type Accessor,
} from "solid-js";

export interface RetainedRead<T> {
  data: Accessor<T | undefined>;
  error: Accessor<string | null>;
  loading: Accessor<boolean>;
  stale: Accessor<boolean>;
  retry: () => Promise<void>;
}

/**
 * A read whose last successful answer remains visible after a failed refresh.
 *
 * Empty data is still data. Failure is a separate state, and a retained answer
 * is explicitly stale until a later successful retry replaces it.
 */
export function createRetainedRead<K, T>(
  source: Accessor<K | false | null | undefined>,
  read: (key: K) => Promise<T>,
  describeError: (error: unknown) => string,
): RetainedRead<T> {
  const [data, setData] = createSignal<T>();
  const [error, setError] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);
  let generation = 0;
  let disposed = false;
  let currentKey: K | false | null | undefined;

  const retry = async (): Promise<void> => {
    const key = source();
    if (key === false || key === null || key === undefined) return;
    const requestGeneration = ++generation;
    setLoading(true);
    try {
      const answer = await read(key);
      if (disposed || requestGeneration !== generation) return;
      setData(() => answer);
      setError(null);
    } catch (cause) {
      if (disposed || requestGeneration !== generation) return;
      setError(describeError(cause));
    } finally {
      if (!disposed && requestGeneration === generation) setLoading(false);
    }
  };

  createEffect(() => {
    const key = source();
    if (!Object.is(key, currentKey)) {
      currentKey = key;
      generation += 1;
      setData(undefined);
      setError(null);
      setLoading(false);
    }
    if (key !== false && key !== null && key !== undefined) void retry();
  });

  onCleanup(() => {
    disposed = true;
    generation += 1;
  });

  return {
    data,
    error,
    loading,
    stale: () => error() !== null && data() !== undefined,
    retry,
  };
}
