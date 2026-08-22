// How old a view is, and the nudge that keeps it from getting old (FR-U10).
//
// FR-U10 is three clauses in one sentence: server-announced state updates
// live from the SSE stream, a lost stream is indicated and reconciles on
// reconnect, and **a stale view shall never present itself as current**. The
// board honoured all three and did it inline, so the other four Vogt surfaces
// honoured none of them — a backlog tab left open all morning looked exactly
// like one loaded a second ago, which is the failure the last clause is
// about. This module is the board's own machinery lifted out so there is one
// implementation of it and not five dialects.
//
// Three decisions worth the reader's time:
//
//  1. **Age is a property of the view, not of the data.** Vogt already
//     reports how old its *evidence* is — the freshness block on every
//     aggregate, which `Backlog.tsx` and `Projects.tsx` render and which this
//     module deliberately does not touch. "This answer came from a sweep an
//     hour ago" and "this page last spoke to Vogt an hour ago" are different
//     facts and a reader needs both: a fresh sweep rendered by a tab that
//     stopped asking is still a stale screen.
//
//  2. **The tone says what will happen next, not just what is true.** A view
//     that polls says so; one that only listens to the stream says so; one
//     that does neither says "press Refresh", because a stale badge that
//     never resolves and never tells the reader what would resolve it is a
//     nag rather than an indication. What is never said is "current" when the
//     view has no reason to believe it is.
//
//  3. **The stream is a nudge, never a payload.** `vogt-changed` carries a
//     cursor and nothing else, so a subscriber re-reads what it cares about
//     through the ordinary call it already makes. Nothing here renders an
//     event, which is what keeps a dropped stream a freshness problem rather
//     than a correctness one.
//
// The subscription guards are the board's, unchanged and now shared: a hidden
// tab is not a view anybody is being misled by (and is reconciled the moment
// it comes back), and a re-read never races a write the user is composing —
// a refetch swaps the rows underneath a half-typed reason and throws it away.

import {
  Component,
  createEffect,
  createSignal,
  onCleanup,
  onMount,
  type Accessor,
} from "solid-js";
import { onVogtChanged } from "./store";

/** How old a view with no poll may be before it stops calling itself current. */
export const DEFAULT_STALE_AFTER_MS = 60_000;

/** What the badge is claiming, as a class suffix and as a decision. */
export type AgeTone = "waiting" | "live" | "paused" | "stale" | "outage";

export interface ViewAge {
  tone: AgeTone;
  text: string;
}

export interface AgeInput {
  /** When this view last got an answer it believes, or `null` if never. */
  loadedAt: number | null;
  /** Now, as a ticking signal's value — see `createNow`. */
  now: number;
  /** The server's own words when Vogt cannot be asked at all (FR-U21). */
  outage?: string | null;
  /** The last attempt failed, but the view still holds an older answer. */
  failed?: boolean;
  /** Seconds between automatic refreshes. `0` is paused; absent is no poll. */
  poll?: number;
  /** This view re-reads on `vogt-changed`. */
  live?: boolean;
  /** Override the age at which a view stops calling itself current. */
  staleAfterMs?: number;
}

export function formatAgo(ms: number): string {
  const seconds = Math.max(0, Math.round(ms / 1000));
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

export function formatClock(at: number): string {
  try {
    return new Date(at).toLocaleTimeString();
  } catch {
    return String(at);
  }
}

/**
 * One view's age, in the words it is rendered with.
 *
 * Pure, and exported for that reason: this is the sentence the requirement is
 * actually about, and it is the part worth asserting without a browser.
 */
export function describeAge(input: AgeInput): ViewAge {
  const at = input.loadedAt;
  if (input.outage) {
    return {
      tone: "outage",
      text: at
        ? `Vogt unreachable — last answer ${formatClock(at)}, not current`
        : "Vogt unreachable",
    };
  }
  if (at === null) return { tone: "waiting", text: "Not loaded yet" };

  const ago = formatAgo(input.now - at);
  if (input.poll === 0) return { tone: "paused", text: `Paused — updated ${ago}` };

  // "Retrying" is a promise, so it is only made by a view that will.
  const retries = input.poll !== undefined || input.live === true;
  if (input.failed) {
    return {
      tone: "stale",
      text: `Stale — updated ${ago}, ${retries ? "retrying" : "press Refresh"}`,
    };
  }

  const limit =
    input.staleAfterMs ??
    (input.poll === undefined ? DEFAULT_STALE_AFTER_MS : input.poll * 3000);
  const overdue = input.now - at > limit;

  if (input.poll !== undefined) {
    return {
      tone: overdue ? "stale" : "live",
      text: `${overdue ? "Stale" : "Polling"} — updated ${ago}`,
    };
  }
  if (overdue) {
    // A stream that died looks exactly like a stream with nothing to say, so
    // a listening view past its limit is stale rather than quiet.
    return {
      tone: "stale",
      text: `Stale — updated ${ago}, ${input.live ? "refresh to be sure" : "press Refresh"}`,
    };
  }
  return {
    tone: "live",
    text: input.live ? `Live — updated ${ago}` : `Updated ${ago}`,
  };
}

/** A clock that ticks while the caller is mounted, and stops when it is not. */
export function createNow(periodMs = 1000): Accessor<number> {
  const [now, setNow] = createSignal(Date.now());
  onMount(() => {
    const tick = window.setInterval(() => setNow(Date.now()), periodMs);
    onCleanup(() => window.clearInterval(tick));
  });
  return now;
}

/**
 * When a resource last produced an answer this view believes.
 *
 * The Vogt surfaces wrap their reads in an `attempt`-style result rather than
 * letting the resource reject, so a failure is a *value* here. Only a
 * successful one moves the stamp: after a failed refresh the view is still
 * showing the previous answer, and saying it was updated just now would be
 * the badge lying about the exact case it exists to catch.
 */
export function createLoadStamp<T>(
  source: Accessor<T | undefined | null>,
  succeeded: (value: T) => boolean = () => true,
): Accessor<number | null> {
  const [at, setAt] = createSignal<number | null>(null);
  createEffect(() => {
    const value = source();
    if (value === undefined || value === null) return;
    if (!succeeded(value)) return;
    setAt(Date.now());
  });
  return at;
}

/** The whole badge: one accessor, clock included. */
export function createViewAge(input: () => Omit<AgeInput, "now">): Accessor<ViewAge> {
  const now = createNow();
  return () => describeAge({ ...input(), now: now() });
}

/** Which `.surface-header-honesty--*` tone a view's age maps to (rail-spec.md
 *  A1). `paused` and `stale` share a colour — both mean "provisional, not
 *  wrong" — and `waiting` shares outage's, since a view that never loaded has
 *  nothing to call current either. */
export function honestyToneClass(tone: AgeTone): string {
  switch (tone) {
    case "live":
      return "surface-header-honesty--fresh";
    case "paused":
      return "surface-header-honesty--partial";
    case "stale":
      return "surface-header-honesty--stale";
    case "outage":
      return "surface-header-honesty--outage";
    case "waiting":
      return "surface-header-honesty--never";
  }
}

/** The badge itself, so five surfaces cannot drift into five renderings. */
export const ViewAgeBadge: Component<{
  age: ViewAge;
  /** Extra classes, for a surface that already styles its own header row. */
  class?: string;
  title?: string;
}> = (props) => (
  <span
    class={`vogt-age vogt-age--${props.age.tone}${props.class ? ` ${props.class}` : ""}`}
    title={props.title}
  >
    {props.age.text}
  </span>
);

export interface LiveOptions {
  /** Re-read only when this says so — a composer open is a reason not to. */
  when?: () => boolean;
  /** Also reconcile when the tab comes back to the front. Default `true`. */
  onVisible?: boolean;
  /** Re-read on a stream nudge. Default `true`. A ranked view sets this
   *  `false`: it does not re-rank the estate under the reader on every
   *  announced change, but a tab returning from the background is still the
   *  moment its answer is furthest from current, so `onVisible` stays on. */
  onNudge?: boolean;
}

/**
 * Re-read when vogt-core says something changed (FR-U10).
 *
 * The front door follows the core's `events.list` cursor and republishes each
 * change onto the stream this client already has open, so a change somebody
 * else made arrives here rather than waiting for somebody to press Refresh.
 * Nothing about the event is rendered: the handler re-reads, and the view's
 * own age badge stays the answer to "is this current".
 */
export function onVogtLive(handler: () => void, options: LiveOptions = {}): void {
  const allowed = () => {
    if (typeof document !== "undefined" && document.hidden) return false;
    return options.when ? options.when() : true;
  };
  onMount(() => {
    if (options.onNudge !== false) {
      const stop = onVogtChanged(() => {
        if (!allowed()) return;
        handler();
      });
      onCleanup(stop);
    }

    if (options.onVisible === false || typeof document === "undefined") return;
    // A hidden tab skips the nudges above, so coming back is the moment its
    // answer is furthest from current — this is FR-U10's "reconcile on
    // reconnect" for the reader who never saw the disconnection.
    const onVisibility = () => {
      if (document.visibilityState !== "visible") return;
      if (options.when && !options.when()) return;
      handler();
    };
    document.addEventListener("visibilitychange", onVisibility);
    onCleanup(() => document.removeEventListener("visibilitychange", onVisibility));
  });
}
