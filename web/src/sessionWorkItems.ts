// The work item each session serves, as the rail shows it (WI-998).
//
// The ref is the engine's `work_item` label on the session summary: the core
// writes it on `session.start` and `session.bind_work`, so the rail needs no
// core join to name the item. What the chip adds — the item's title for the
// tooltip and whether it is finished (the chip goes muted) — is read from the
// core once per distinct ref and cached, re-read when vogt-core says a work
// item changed. No core behind the front door means a chip with the ref only:
// the rail keeps working.

import { createStore, produce } from "solid-js/store";
import { VogtUnavailable, listWork } from "./vogtApi";

export interface WorkItemFacts {
  title: string;
  state: string;
}

/** Workflow states a work item is finished in; a chip on one reads muted. */
export const FINISHED_STATES = new Set(["done", "wont_do"]);

const [facts, setFacts] = createStore<Record<string, WorkItemFacts | null>>({});
const inFlight = new Set<string>();
/** When a ref last failed to read, so a core that is down is not asked again
 *  on every render of the rail. */
const failedAt = new Map<string, number>();
const RETRY_AFTER_MS = 60_000;

/** Title and state for a bound ref: `undefined` while unknown (or not
 *  readable), `null` when the core has no such item. */
export function workItemFacts(ref: string): WorkItemFacts | null | undefined {
  const known = facts[ref];
  if (known === undefined) void load(ref);
  return known;
}

/** Forget what is cached, so the next read asks the core again — on a
 *  `vogt-changed` event about a work item. Only refs already read are
 *  re-read, and only when a chip asks for them. */
export function invalidateWorkItemFacts(ref?: string): void {
  if (ref === undefined) failedAt.clear();
  else failedAt.delete(ref);
  setFacts(
    produce((current) => {
      for (const key of Object.keys(current)) {
        if (ref === undefined || key === ref) delete current[key];
      }
    }),
  );
}

async function load(ref: string): Promise<void> {
  if (inFlight.has(ref)) return;
  const failed = failedAt.get(ref);
  if (failed !== undefined && Date.now() - failed < RETRY_AFTER_MS) return;
  inFlight.add(ref);
  try {
    // `query` matches the ref among others; take the exact row.
    const answer = await listWork({
      query: ref,
      include_finished: true,
      limit: 20,
      mode: "summary",
    });
    const row = answer.items.find((item) => item.ref === ref);
    failedAt.delete(ref);
    setFacts(ref, row ? { title: row.title, state: row.state } : null);
  } catch (error) {
    failedAt.set(ref, Date.now());
    // No core, or it refused: the chip shows the ref alone. Not cached, so a
    // later read can try again once the core is back.
    if (!(error instanceof VogtUnavailable)) {
      console.warn(`work item ${ref} could not be read for its chip`, error);
    }
  } finally {
    inFlight.delete(ref);
  }
}

/** Keep the rail's chips current from vogt-core's change stream: a bind or
 *  unbind made anywhere (an agent over MCP, another tab) re-reads the
 *  engine's session list, whose labels the engine changed without an event
 *  of its own; a work item edited or moved drops the cached title and state.
 *  Coalesced, so a burst of changes costs one read. Returns the unsubscribe. */
export function watchSessionWorkItems(
  subscribe: (listener: (event: { kind: string }) => void) => () => void,
  refreshSessions: () => Promise<void>,
): () => void {
  let sessionsTimer: ReturnType<typeof setTimeout> | undefined;
  let factsTimer: ReturnType<typeof setTimeout> | undefined;
  const unsubscribe = subscribe((event) => {
    if (event.kind === "session.work_bound" || event.kind === "session.work_unbound") {
      clearTimeout(sessionsTimer);
      sessionsTimer = setTimeout(() => void refreshSessions().catch(() => {}), 250);
    } else if (event.kind === "work.updated" || event.kind === "work.transitioned") {
      clearTimeout(factsTimer);
      factsTimer = setTimeout(() => invalidateWorkItemFacts(), 2_000);
    }
  });
  return () => {
    clearTimeout(sessionsTimer);
    clearTimeout(factsTimer);
    unsubscribe();
  };
}
