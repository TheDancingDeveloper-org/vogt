/** Shared, typed, identity-scoped taxonomy reads for the PWA (#417). */

import {
  getBase,
  getToken,
  subscribeAuthRejected,
  subscribeAuthState,
} from "./api";
import { onVogtChangedEvent, type VogtChangedEvent } from "./store";
import {
  registerTaxonomyInvalidator,
  type TaxonomyKind,
} from "./taxonomyInvalidation";
import {
  listActors,
  listInitiatives,
  listLabels,
  listProjects,
  listWorkflows,
  type ProjectListEntry,
  type Workflow,
} from "./vogtApi";

export type TaxonomyValues = {
  projects: { projects: ProjectListEntry[]; total: number };
  actors: { actors: { identity_ref: string; display_name: string }[] };
  workflows: { workflows: Workflow[] };
  labels: { labels: { name: string; color?: string }[] };
  initiatives: { initiatives: { id: string; slug: string; title: string }[] };
};

const TAXONOMY_KINDS: readonly TaxonomyKind[] = [
  "projects",
  "actors",
  "workflows",
  "labels",
  "initiatives",
];

const ENTITY_KIND_TO_TAXONOMY: Readonly<Record<string, TaxonomyKind>> = {
  project: "projects",
  projects: "projects",
  actor: "actors",
  actors: "actors",
  workflow: "workflows",
  workflows: "workflows",
  label: "labels",
  labels: "labels",
  initiative: "initiatives",
  initiatives: "initiatives",
};

/** A quiet client still revalidates eventually if an external writer emits no event. */
export const TAXONOMY_TTL_MS = 5 * 60 * 1000;

interface Entry<T> {
  value?: T;
  promise?: Promise<T>;
  fetchedAt: number;
  sequence: number;
  generation: number;
}

const entries = new Map<string, Entry<unknown>>();
const latestSequence = new Map<TaxonomyKind, number>();
const generations = new Map<TaxonomyKind, number>();

function generation(kind: TaxonomyKind): number {
  return generations.get(kind) ?? 0;
}

function identity(): string {
  // Credentials and the configured endpoint form the browser identity. The
  // delimiter makes otherwise ambiguous concatenations distinct.
  return `${getBase()}\u0000${getToken()}`;
}

function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value !== null && typeof value === "object") {
    const object = value as Record<string, unknown>;
    return `{${Object.keys(object)
      .sort()
      .filter((key) => object[key] !== undefined)
      .map((key) => `${JSON.stringify(key)}:${canonical(object[key])}`)
      .join(",")}}`;
  }
  return JSON.stringify(value);
}

function cacheKey(kind: TaxonomyKind, params: Record<string, unknown>): string {
  return `${identity()}\u0000${kind}\u0000${canonical(params)}`;
}

function invalidate(kind?: TaxonomyKind): void {
  if (kind === undefined) {
    for (const one of TAXONOMY_KINDS) generations.set(one, generation(one) + 1);
    entries.clear();
    return;
  }
  generations.set(kind, generation(kind) + 1);
  for (const key of entries.keys()) {
    if (key.includes(`\u0000${kind}\u0000`)) entries.delete(key);
  }
}

/** Explicit invalidation used by successful taxonomy/project mutations. */
export function clearTaxonomy(kind?: TaxonomyKind): void {
  invalidate(kind);
}

/** Reset hook for deterministic tests. */
export function clearTaxonomyCache(): void {
  invalidate();
  latestSequence.clear();
}

/** Apply only the matching entity-kind's sequence gate. */
export function noteTaxonomyChange(entityKind: string, sequence: number): void {
  const kind = ENTITY_KIND_TO_TAXONOMY[entityKind];
  if (!kind) return;
  latestSequence.set(kind, Math.max(latestSequence.get(kind) ?? 0, sequence));
}

function read<K extends TaxonomyKind>(
  kind: K,
  params: Record<string, unknown>,
  fetcher: () => Promise<TaxonomyValues[K]>,
): Promise<TaxonomyValues[K]> {
  const key = cacheKey(kind, params);
  const now = Date.now();
  const currentGeneration = generation(kind);
  const currentSequence = latestSequence.get(kind) ?? 0;
  const found = entries.get(key) as Entry<TaxonomyValues[K]> | undefined;

  // Single-flight applies to stale entries too: an invalidation can cause
  // many mounted surfaces to revalidate, but only the first makes a request.
  if (found?.generation === currentGeneration && found.promise) return found.promise;
  if (
    found?.generation === currentGeneration &&
    found.value !== undefined &&
    found.sequence >= currentSequence &&
    now - found.fetchedAt < TAXONOMY_TTL_MS
  ) {
    return Promise.resolve(found.value);
  }

  const entry: Entry<TaxonomyValues[K]> = {
    fetchedAt: now,
    sequence: currentSequence,
    generation: currentGeneration,
  };
  const promise = fetcher()
    .then((value) => {
      entry.value = value;
      entry.fetchedAt = Date.now();
      entry.promise = undefined;
      // A write or identity change may have happened while this request was
      // pending. Its answer must not become current again.
      if (entry.generation === generation(kind) && entries.get(key) === entry) {
        entries.set(key, entry);
      }
      return value;
    })
    .catch((error: unknown) => {
      // Outages and ordinary failures are not taxonomy and are retryable.
      if (entries.get(key) === entry) entries.delete(key);
      throw error;
    });
  entry.promise = promise;
  entries.set(key, entry);
  return promise;
}

export const taxonomy = {
  projects: (params: Record<string, unknown> = { limit: 200 }) =>
    read("projects", params, () => listProjects(params)),
  actors: () => read("actors", {}, () => listActors()),
  workflows: () => read("workflows", {}, () => listWorkflows()),
  labels: () => read("labels", {}, () => listLabels()),
  initiatives: () => read("initiatives", {}, () => listInitiatives()),
};

// One process-wide cache, not one cache per mounted surface.
const stopEvents = onVogtChangedEvent((event: VogtChangedEvent) => {
  noteTaxonomyChange(event.entity_kind, event.seq);
});
const stopAuthState = subscribeAuthState(clearTaxonomyCache);
const stopAuthRejected = subscribeAuthRejected(() => clearTaxonomyCache());
registerTaxonomyInvalidator(clearTaxonomy);

void stopEvents;
void stopAuthState;
void stopAuthRejected;
