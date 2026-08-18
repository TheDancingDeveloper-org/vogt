import { createStore } from "solid-js/store";
import { backlog, listInbox, listProjects, listWork } from "./vogtApi";

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
}

const initial = (): PlaceMetric => ({ value: null, state: "loading" });

function requiredCount(value: number | undefined, source: string): number {
  if (typeof value !== "number") throw new Error(`${source} did not report a total`);
  return value;
}

/**
 * One shell-owned set of glanceable counts for desktop and phone navigation.
 * Each number comes from the same canonical read as its surface. Providers
 * settle independently so a core outage never turns an unknown value into 0.
 */
export function createPlaceMetrics(): {
  metrics: PlaceMetrics;
  refresh: () => Promise<void>;
} {
  const [metrics, setMetrics] = createStore<PlaceMetrics>({
    inbox: initial(),
    projects: initial(),
    board: initial(),
    backlog: initial(),
  });
  let generation = 0;

  const load = async (
    name: keyof PlaceMetrics,
    read: () => Promise<number>,
    currentGeneration: number,
  ) => {
    try {
      const value = await read();
      if (generation !== currentGeneration) return;
      setMetrics(name, { value, state: "ready" });
    } catch {
      if (generation !== currentGeneration) return;
      setMetrics(name, "state", "unavailable");
    }
  };

  const refresh = async () => {
    const currentGeneration = ++generation;
    for (const name of ["inbox", "projects", "board", "backlog"] as const) {
      setMetrics(name, "state", metrics[name].value === null ? "loading" : "stale");
    }
    await Promise.all([
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
    ]);
  };

  return { metrics, refresh };
}
