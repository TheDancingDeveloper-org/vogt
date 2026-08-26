// Lazily shared assistant reads (#416).
//
// Assistant history is session-scoped, not component-scoped. Keeping the
// request and its result here prevents the Sessions shell and an Assistant tab
// (or two retained instances) from asking the engine the same question. This
// is intentionally assistant-specific, not a general HTTP cache.

import {
  api,
  type AssistantHistory,
  type AssistantPendingAction,
  type AssistantTranscriptEntry,
} from "./api";
import { listProjects, type ProjectListEntry } from "./vogtApi";
import { setPendingAction } from "./pendingAction";

export interface AssistantSnapshot {
  transcript: AssistantTranscriptEntry[];
  pendingAction: AssistantPendingAction | null;
  projects: ProjectListEntry[];
  fetchedAt: number;
}

let snapshot: AssistantSnapshot | null = null;
let flight: Promise<AssistantSnapshot> | null = null;
let flightGeneration: number | null = null;
let generation = 0;

/** Read once per invalidation, with concurrent callers sharing the flight. */
export function readAssistantSnapshot(
  assistantEnabled: boolean,
): Promise<AssistantSnapshot | null> {
  if (!assistantEnabled) return Promise.resolve(null);
  if (snapshot) return Promise.resolve(snapshot);
  if (flight && flightGeneration === generation) return flight;

  const startedAt = generation;
  flight = (async () => {
    // Project names are only used by voice repair. They are best-effort: a
    // core outage must not turn a usable assistant transcript into an error.
    const projects = listProjects()
      .then((result) => result.projects)
      .catch(() => [] as ProjectListEntry[]);
    // A deployment can advertise the shell before the assistant route is
    // provisioned, and older engines answer this read with 404. Treat that
    // as an empty snapshot so project vocabulary still reaches voice repair;
    // the assistant composer remains usable and no cache consumer fans out a
    // second history request to recover.
    const history: AssistantHistory = await api.assistantHistory().catch(() => ({
      transcript: [],
      pending_action: undefined,
    }));
    const next: AssistantSnapshot = {
      transcript: history.transcript,
      pendingAction: history.pending_action ?? null,
      projects: await projects,
      fetchedAt: Date.now(),
    };
    // A mutation can invalidate a flight while it is in progress. Do not let
    // that stale answer repopulate the shared cache or pending-action signal.
    if (startedAt === generation) {
      snapshot = next;
      setPendingAction(next.pendingAction);
    }
    return next;
  })().finally(() => {
    if (flightGeneration === startedAt) {
      flight = null;
      flightGeneration = null;
    }
  });
  flightGeneration = startedAt;
  return flight;
}

/** Make the next assistant open obtain fresh history and pending action. */
export function invalidateAssistantSnapshot(): void {
  generation += 1;
  snapshot = null;
}

/** Schedule hydration after the current route has had a chance to attach. */
export function deferAssistantHydration(task: () => void): () => void {
  let cancelled = false;
  const run = () => {
    if (!cancelled) task();
  };

  const idleWindow = window as Window & {
    requestIdleCallback?: (callback: () => void) => number;
    cancelIdleCallback?: (id: number) => void;
  };
  if (idleWindow.requestIdleCallback) {
    const id = idleWindow.requestIdleCallback(run);
    return () => {
      cancelled = true;
      idleWindow.cancelIdleCallback?.(id);
    };
  }

  const id = setTimeout(run, 0);
  return () => {
    cancelled = true;
    clearTimeout(id);
  };
}
