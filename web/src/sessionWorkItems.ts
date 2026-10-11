// The work item each session serves, as the rail shows it (WI-998).
//
// The ref is the engine's `work_item` label on the session summary: the core
// writes it on `session.start` and `session.bind_work`, so the rail needs no
// core join to name the item. What the chip adds — the item's title for the
// tooltip and whether it is finished (the chip goes muted) — is read from the
// core and cached, re-read when vogt-core says that item changed. No core
// behind the front door means a chip with the ref only: the rail keeps
// working.
//
// One rail render is one read (WI-1224). The chips used to ask one
// `work.list` search each, all at once — about fifty on a busy rail — and
// every work-item event anywhere dropped every cached chip, so the whole
// fan-out repeated while agents edited items. On its own that overran the
// front door's in-flight cap, and the shed requests starved the reads the
// rest of the shell needed. Now the refs a render asks for are collected and
// read together with `work.get_many`, and an event drops only the refs it
// names.

import { createStore, produce } from "solid-js/store";
import { VogtBusy, VogtUnavailable, WORK_GET_MANY_LIMIT, getWorkMany } from "./vogtApi";

export interface WorkItemFacts {
  title: string;
  state: string;
}

/** Workflow states a work item is finished in; a chip on one reads muted. */
export const FINISHED_STATES = new Set(["done", "wont_do"]);

const [facts, setFacts] = createStore<Record<string, WorkItemFacts | null>>({});
/** Refs asked for since the last read went out. */
const pending = new Set<string>();
let flushScheduled = false;
const inFlight = new Set<string>();
/** How many times each ref has been invalidated, so a read that was already
 *  in the air when its item changed is not cached as the new state. */
const generation = new Map<string, number>();
let generationAll = 0;
/** When a ref last failed to read, so a core that is down is not asked again
 *  on every render of the rail. */
const failedAt = new Map<string, number>();
/** Refs whose last failure was a door still busy after `call()`'s own
 *  retries: asked again sooner than an outage, because busy clears itself. */
const busyRefs = new Set<string>();
const RETRY_AFTER_MS = 60_000;
const BUSY_RETRY_AFTER_MS = 10_000;

/** Title and state for a bound ref: `undefined` while unknown (or not
 *  readable), `null` when the core has no such item. */
export function workItemFacts(ref: string): WorkItemFacts | null | undefined {
  const known = facts[ref];
  if (known === undefined) request(ref);
  return known;
}

/** Forget what is cached for these refs, so the next read asks the core
 *  again — on a `vogt-changed` event about a work item. With no refs, forget
 *  everything. Only refs a chip asks for again are re-read. */
export function invalidateWorkItemFacts(refs?: Iterable<string>): void {
  const only = refs === undefined ? undefined : new Set(refs);
  if (only === undefined) {
    generationAll += 1;
    failedAt.clear();
    busyRefs.clear();
  } else {
    for (const ref of only) {
      generation.set(ref, (generation.get(ref) ?? 0) + 1);
      failedAt.delete(ref);
      busyRefs.delete(ref);
    }
  }
  setFacts(
    produce((current) => {
      for (const key of Object.keys(current)) {
        if (only === undefined || only.has(key)) delete current[key];
      }
    }),
  );
}

function generationOf(ref: string): string {
  return `${generationAll}.${generation.get(ref) ?? 0}`;
}

function request(ref: string): void {
  if (inFlight.has(ref) || pending.has(ref)) return;
  const failed = failedAt.get(ref);
  const wait = busyRefs.has(ref) ? BUSY_RETRY_AFTER_MS : RETRY_AFTER_MS;
  if (failed !== undefined && Date.now() - failed < wait) return;
  pending.add(ref);
  if (flushScheduled) return;
  flushScheduled = true;
  // Every chip of one render asks in the same task; the read goes out once
  // they all have.
  queueMicrotask(flush);
}

function flush(): void {
  flushScheduled = false;
  const refs = [...pending];
  pending.clear();
  for (let at = 0; at < refs.length; at += WORK_GET_MANY_LIMIT) {
    void load(refs.slice(at, at + WORK_GET_MANY_LIMIT));
  }
}

async function load(refs: string[]): Promise<void> {
  const asked = new Map(refs.map((ref) => [ref, generationOf(ref)]));
  const superseded: string[] = [];
  for (const ref of refs) inFlight.add(ref);
  try {
    const answer = await getWorkMany(refs);
    const found = new Map(answer.items.map((item) => [item.ref, item]));
    setFacts(
      produce((current) => {
        for (const ref of refs) {
          // Changed while this read was in the air: what came back may be
          // from before the change, so ask again rather than cache it.
          if (asked.get(ref) !== generationOf(ref)) {
            superseded.push(ref);
            continue;
          }
          const row = found.get(ref);
          failedAt.delete(ref);
          busyRefs.delete(ref);
          // A ref the core does not name comes back in `missing`.
          current[ref] = row ? { title: row.title, state: row.state } : null;
        }
      }),
    );
  } catch (error) {
    const now = Date.now();
    for (const ref of refs) {
      if (asked.get(ref) !== generationOf(ref)) {
        superseded.push(ref);
        continue;
      }
      failedAt.set(ref, now);
      if (error instanceof VogtBusy) busyRefs.add(ref);
      else busyRefs.delete(ref);
    }
    // No core, a door still busy, or a refusal: the chips show the ref
    // alone. Not cached, so a later read can try again.
    if (!(error instanceof VogtUnavailable) && !(error instanceof VogtBusy)) {
      console.warn(`work items ${refs.join(", ")} could not be read for their chips`, error);
    }
  } finally {
    for (const ref of refs) inFlight.delete(ref);
  }
  for (const ref of superseded) request(ref);
}

/** What the watcher reads off a `vogt-changed` event. */
export interface WorkItemChange {
  kind: string;
  summary?: unknown;
}

/** The ref a work-item event names, from the core's event summary. */
function changedRef(event: WorkItemChange): string | undefined {
  const summary = event.summary;
  if (summary === null || typeof summary !== "object") return undefined;
  const ref = (summary as { ref?: unknown }).ref;
  return typeof ref === "string" && ref ? ref : undefined;
}

/** Keep the rail's chips current from vogt-core's change stream: a bind or
 *  unbind made anywhere (an agent over MCP, another tab) re-reads the
 *  engine's session list, whose labels the engine changed without an event
 *  of its own; a work item edited or moved drops that item's cached title
 *  and state. Coalesced, so a burst of changes costs one read. Returns the
 *  unsubscribe. */
export function watchSessionWorkItems(
  subscribe: (listener: (event: WorkItemChange) => void) => () => void,
  refreshSessions: () => Promise<void>,
): () => void {
  let sessionsTimer: ReturnType<typeof setTimeout> | undefined;
  let factsTimer: ReturnType<typeof setTimeout> | undefined;
  const changed = new Set<string>();
  // An event that does not say which item changed costs every chip, as
  // every event used to.
  let changedUnknown = false;
  const unsubscribe = subscribe((event) => {
    if (event.kind === "session.work_bound" || event.kind === "session.work_unbound") {
      clearTimeout(sessionsTimer);
      sessionsTimer = setTimeout(() => void refreshSessions().catch(() => {}), 250);
    } else if (event.kind === "work.updated" || event.kind === "work.transitioned") {
      const ref = changedRef(event);
      if (ref === undefined) changedUnknown = true;
      else changed.add(ref);
      clearTimeout(factsTimer);
      factsTimer = setTimeout(() => {
        invalidateWorkItemFacts(changedUnknown ? undefined : changed);
        changed.clear();
        changedUnknown = false;
      }, 2_000);
    }
  });
  return () => {
    clearTimeout(sessionsTimer);
    clearTimeout(factsTimer);
    unsubscribe();
  };
}
