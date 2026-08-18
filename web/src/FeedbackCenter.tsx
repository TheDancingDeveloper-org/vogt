import { type Component, For, Show, createSignal, onCleanup } from "solid-js";

export type FeedbackKind = "info" | "success" | "error";

export interface FeedbackOptions {
  kind?: FeedbackKind;
  /** Stable keys replace in place; otherwise identical messages coalesce. */
  key?: string;
  details?: string;
  actionLabel?: string;
  action?: () => void | Promise<void>;
  /** Errors persist by default. Set an explicit duration only when safe. */
  durationMs?: number | null;
}

export interface FeedbackItem extends FeedbackOptions {
  id: string;
  key: string;
  message: string;
  kind: FeedbackKind;
  count: number;
}

export interface FeedbackQueue {
  items: () => FeedbackItem[];
  push: (message: string, options?: FeedbackOptions) => string;
  dismiss: (id: string) => void;
  clear: () => void;
}

export function createFeedbackQueue(): FeedbackQueue {
  const [items, setItems] = createSignal<FeedbackItem[]>([]);
  const timers = new Map<string, number>();
  let sequence = 0;

  const cancelTimer = (id: string) => {
    const timer = timers.get(id);
    if (timer !== undefined) window.clearTimeout(timer);
    timers.delete(id);
  };

  const dismiss = (id: string) => {
    cancelTimer(id);
    setItems((current) => current.filter((item) => item.id !== id));
  };

  const schedule = (item: FeedbackItem) => {
    cancelTimer(item.id);
    const duration = item.durationMs === undefined
      ? item.kind === "error"
        ? null
        : 4000
      : item.durationMs;
    if (duration !== null) {
      timers.set(item.id, window.setTimeout(() => dismiss(item.id), duration));
    }
  };

  const push = (message: string, options: FeedbackOptions = {}): string => {
    const kind = options.kind ?? "info";
    const key = options.key ?? `${kind}:${message}`;
    const existing = items().find((item) => item.key === key);
    if (existing) {
      const replacement: FeedbackItem = {
        ...existing,
        ...options,
        message,
        kind,
        count: existing.message === message ? existing.count + 1 : 1,
      };
      setItems((current) =>
        current.map((item) => item.id === existing.id ? replacement : item),
      );
      schedule(replacement);
      return existing.id;
    }

    sequence += 1;
    const item: FeedbackItem = {
      ...options,
      id: `feedback-${sequence}`,
      key,
      message,
      kind,
      count: 1,
    };
    setItems((current) => {
      const next = [...current, item];
      const informational = next.filter((entry) => entry.kind !== "error");
      const overflow = Math.max(0, informational.length - 3);
      if (overflow === 0) return next;
      const remove = new Set(informational.slice(0, overflow).map((entry) => entry.id));
      for (const id of remove) cancelTimer(id);
      return next.filter((entry) => !remove.has(entry.id));
    });
    schedule(item);
    return item.id;
  };

  const clear = () => {
    for (const id of timers.keys()) cancelTimer(id);
    setItems([]);
  };

  onCleanup(clear);
  return { items, push, dismiss, clear };
}

interface Props {
  queue: FeedbackQueue;
}

const kindLabel: Record<FeedbackKind, string> = {
  info: "Info",
  success: "Success",
  error: "Error",
};

const FeedbackCenter: Component<Props> = (props) => (
  <section class="feedback-center" aria-label="Notifications">
    <For each={props.queue.items()}>
      {(item) => (
        <article
          class={`feedback-item ${item.kind}`}
          role={item.kind === "error" ? "alert" : "status"}
          aria-live={item.kind === "error" ? "assertive" : "polite"}
          aria-atomic="true"
        >
          <div class="feedback-copy">
            <span class="feedback-kind">{kindLabel[item.kind]}</span>
            <span class="feedback-message">
              {item.message}
              <Show when={item.count > 1}> ×{item.count}</Show>
            </span>
            <Show when={item.details}>
              <details class="feedback-details">
                <summary>Open details</summary>
                <div>{item.details}</div>
              </details>
            </Show>
          </div>
          <div class="feedback-actions">
            <Show when={item.action && item.actionLabel}>
              <button
                type="button"
                onClick={() => void item.action?.()}
              >
                {item.actionLabel}
              </button>
            </Show>
            <button
              type="button"
              aria-label={`Dismiss ${item.message}`}
              onClick={() => props.queue.dismiss(item.id)}
            >
              Dismiss
            </button>
          </div>
        </article>
      )}
    </For>
  </section>
);

export default FeedbackCenter;
