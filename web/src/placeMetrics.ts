import { createStore } from "solid-js/store";
import { ApiError } from "./api";
import {
  backlog,
  listDrift,
  listInbox,
  listProjects,
  listWork,
  placeMetrics as readPlaceMetrics,
} from "./vogtApi";

export type PlaceMetricState = "loading" | "ready" | "stale" | "unavailable";

export interface PlaceMetric {
  value: number | null;
  state: PlaceMetricState;
}

export interface PlaceMetrics {
  inbox: PlaceMetric;
  projects: PlaceMetric;
  board: PlaceMetric;
  backlog: PlaceMetric;
  /**
   * Whether the estate currently carries any drift proposal. The rail colours
   * Projects amber when this is non-zero (design 5b / 4a rule 4: "outlined
   * --activity-running = drift"). It is an existence signal, not a per-project
   * count, so the read stays bounded (`limit: 1`) like its four siblings.
   */
  drift: PlaceMetric;
}

const initial = (): PlaceMetric => ({ value: null, state: "loading" });

function requiredCount(value: number | undefined, source: string): number {
  if (typeof value !== "number") throw new Error(`${source} did not report a total`);
  return value;
}

function metricValue(value: number | null | undefined): number | null {
  return typeof value === "number" ? value : null;
}

export interface PlaceMetricsController {
  metrics: PlaceMetrics;
  /** Read now, and settle before the promise does. Sign-in and first paint. */
  refresh: () => Promise<void>;
  /** Something changed. Coalesces a burst of events into one read (#138). */
  nudge: () => void;
  /** Drop any scheduled read; the shell owns these for its whole lifetime. */
  dispose: () => void;
}

/**
 * How long a burst of `vogt-changed` events is allowed to collect before the
 * counts are re-read. Long enough that a transition, its audit row and its
 * event are one read rather than three; short enough that a badge is never
 * visibly behind the surface beside it.
 */
const COALESCE_MS = 750;

/**
 * How long the badges wait before asking again after a read that failed, and
 * the ceiling that wait grows to. A read that fails is most often a read that
 * exceeded its deadline, and a deadline without backoff is a retry loop: on
 * prod the badge aggregate took 9 s against an 8 s deadline, so every tab
 * aborted and re-asked every 8 s while the core still ran each abandoned read
 * to completion — until nothing else could get through (#581). The first
 * wait matches the read deadline; each failure doubles it.
 */
export const RETRY_BASE_MS = 8_000;
export const RETRY_CAP_MS = 120_000;

/**
 * One shell-owned set of glanceable counts for desktop and phone navigation.
 * Each number comes from the same canonical read as its surface. Providers
 * settle independently so a core outage never turns an unknown value into 0.
 *
 * Four counts is four round trips, so *when* they are read matters as much as
 * what they read. Every `vogt-changed` event used to refresh them immediately
 * in every open tab, which turned one write into 4N queries and made the core
 * spend 88% of its requests on badges (#138). Two rules now hold that down,
 * and both live here rather than at the call site so a second consumer cannot
 * reintroduce the fan-out: at most one read is in the air at a time, and
 * event-driven reads are coalesced over `COALESCE_MS`. The third rule — a
 * hidden tab does not read at all — belongs to `onVogtLive`, which is what
 * the shell subscribes with.
 */
export function createPlaceMetrics(): PlaceMetricsController {
  const [metrics, setMetrics] = createStore<PlaceMetrics>({
    inbox: initial(),
    projects: initial(),
    board: initial(),
    backlog: initial(),
    drift: initial(),
  });
  let generation = 0;
  let aggregateSupported: boolean | null = null;
  let failures = 0;
  let retryAt = 0;
  let retryTimer: ReturnType<typeof setTimeout> | null = null;

  const names = ["inbox", "projects", "board", "backlog", "drift"] as const;

  /**
   * A read did not answer. Keep what is known, labelled as such: a count from
   * a minute ago is worth more than a dash, and a value only becomes
   * "unavailable" when there has never been one. Then hold off — nudges
   * during the wait are dropped, and one timed read brings the badges back
   * without needing an event to happen first.
   */
  const failed = () => {
    for (const name of names) {
      setMetrics(name, "state", metrics[name].value === null ? "unavailable" : "stale");
    }
    failures += 1;
    const wait = Math.min(RETRY_BASE_MS * 2 ** (failures - 1), RETRY_CAP_MS);
    retryAt = Date.now() + wait;
    if (retryTimer !== null) clearTimeout(retryTimer);
    retryTimer = setTimeout(() => {
      retryTimer = null;
      void refresh();
    }, wait);
  };

  const succeeded = () => {
    failures = 0;
    retryAt = 0;
    if (retryTimer !== null) {
      clearTimeout(retryTimer);
      retryTimer = null;
    }
  };

  const load = async (
    name: keyof PlaceMetrics,
    read: () => Promise<number>,
    currentGeneration: number,
  ): Promise<boolean> => {
    try {
      const value = await read();
      if (generation !== currentGeneration) return true;
      setMetrics(name, { value, state: "ready" });
      return true;
    } catch {
      if (generation !== currentGeneration) return true;
      setMetrics(name, "state", "unavailable");
      return false;
    }
  };

  const read = async () => {
    const currentGeneration = ++generation;
    for (const name of ["inbox", "projects", "board", "backlog", "drift"] as const) {
      setMetrics(name, "state", metrics[name].value === null ? "loading" : "stale");
    }
    if (aggregateSupported !== false) {
      try {
        const result = await readPlaceMetrics();
        aggregateSupported = true;
        if (generation !== currentGeneration) return;
        setMetrics("inbox", {
          value: metricValue(result.inbox_active),
          state: result.inbox_active === null || result.inbox_active === undefined ? "unavailable" : "ready",
        });
        setMetrics("projects", {
          value: metricValue(result.projects_total),
          state: result.projects_total === null || result.projects_total === undefined ? "unavailable" : "ready",
        });
        setMetrics("board", {
          value: metricValue(result.work_total),
          state: result.work_total === null || result.work_total === undefined ? "unavailable" : "ready",
        });
        setMetrics("backlog", {
          value: metricValue(result.backlog_total_considered),
          state: result.backlog_total_considered === null || result.backlog_total_considered === undefined ? "unavailable" : "ready",
        });
        setMetrics("drift", {
          value: result.drift_present === null || result.drift_present === undefined ? null : result.drift_present ? 1 : 0,
          state: result.drift_present === null || result.drift_present === undefined ? "unavailable" : "ready",
        });
        succeeded();
        return;
      } catch (error) {
        if (!(error instanceof ApiError) || error.status !== 404) {
          if (generation !== currentGeneration) return;
          failed();
          return;
        }
        // Mixed deployments may have an older core. Detect that once, then
        // keep using the compatible legacy path without retrying the missing
        // operation on every nudge.
        aggregateSupported = false;
      }
    }

    const settled = await Promise.all([
      load(
        "inbox",
        async () => {
          const result = await listInbox({ limit: 1 });
          return requiredCount(result.counts?.active, "Inbox");
        },
        currentGeneration,
      ),
      load(
        "projects",
        async () => requiredCount((await listProjects({ limit: 1 })).total, "Projects"),
        currentGeneration,
      ),
      load(
        "board",
        async () => requiredCount((await listWork({ limit: 1 })).total, "Board"),
        currentGeneration,
      ),
      load(
        "backlog",
        async () => requiredCount((await backlog({ limit: 1 })).total_considered, "Backlog"),
        currentGeneration,
      ),
      load(
        "drift",
        // Existence, not census: one proposal is enough to colour Projects.
        async () => ((await listDrift({ limit: 1 })).proposals.length > 0 ? 1 : 0),
        currentGeneration,
      ),
    ]);
    if (generation !== currentGeneration) return;
    // One provider down is that provider's badge; every provider down is the
    // core not answering, and that is what the backoff is for.
    if (settled.every((ok) => !ok)) failed();
    else succeeded();
  };

  let inFlight: Promise<void> | null = null;
  let followUp = false;
  let timer: ReturnType<typeof setTimeout> | null = null;

  const refresh = (): Promise<void> => {
    if (inFlight) {
      // A read is already in the air, so its answer is about to be one event
      // out of date. Ask for exactly one more pass rather than a second set
      // of four queries racing the first.
      followUp = true;
      return inFlight;
    }
    inFlight = (async () => {
      try {
        do {
          followUp = false;
          await read();
        } while (followUp);
      } finally {
        inFlight = null;
      }
    })();
    return inFlight;
  };

  const nudge = (): void => {
    // A window that starts on the first event of a burst and is not extended
    // by the ones behind it. A resetting debounce would be starved by a busy
    // core — the counts would never be read while anything was happening,
    // which is precisely when they are wrong.
    if (timer !== null) return;
    // Backing off after a failed read: the timed retry owns the next read,
    // and an event arriving in the meantime does not shorten the wait. A
    // core that could not answer eight seconds ago is not helped by being
    // asked again because something else changed (#581).
    if (Date.now() < retryAt) return;
    timer = setTimeout(() => {
      timer = null;
      void refresh();
    }, COALESCE_MS);
  };

  const dispose = (): void => {
    if (retryTimer !== null) {
      clearTimeout(retryTimer);
      retryTimer = null;
    }
    if (timer === null) return;
    clearTimeout(timer);
    timer = null;
  };

  return { metrics, refresh, nudge, dispose };
}
