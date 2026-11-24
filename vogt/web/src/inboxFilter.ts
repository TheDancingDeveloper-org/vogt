// The Inbox's structured filter: which source, whose doing, which triage
// state. Saved per account (`inbox.filter`) so it stays applied until the
// person changes or clears it — across navigation, reloads and devices — and
// the sidebar badge counts under the same filter (the core reads the saved
// value itself for `place.metrics`).
//
// Free-text search is deliberately not part of it: search is a temporary
// narrowing of what is on screen, and the badge never counts by it.

import { accountPreference } from "./accountPrefs";

export const INBOX_SOURCES = ["github", "drift", "ci", "agent"] as const;
export type InboxSourceName = (typeof INBOX_SOURCES)[number];

export const INBOX_ACTORS = ["any", "external", "org", "bot"] as const;
export type InboxActor = (typeof INBOX_ACTORS)[number];

export const INBOX_TRIAGE = ["active", "snoozed", "archived", "all"] as const;
export type InboxTriage = (typeof INBOX_TRIAGE)[number];

export interface InboxFilter {
  /** "" is every source. */
  source: InboxSourceName | "";
  actor: InboxActor;
  triage: InboxTriage;
}

export const DEFAULT_INBOX_FILTER: InboxFilter = { source: "", actor: "any", triage: "active" };

export const ACTOR_LABELS: Record<InboxActor, string> = {
  any: "Anyone",
  external: "External people only",
  org: "Org members",
  bot: "Bots and Vogt",
};

/** Short pill labels for the phone, where the long ones do not fit. */
export const ACTOR_PILLS: Record<InboxActor, string> = {
  any: "Anyone",
  external: "External",
  org: "Org",
  bot: "Bots",
};

export const TRIAGE_LABELS: Record<InboxTriage, string> = {
  active: "Active",
  snoozed: "Snoozed",
  archived: "Archived",
  all: "All states",
};

function oneOf<T extends string>(values: readonly T[], value: unknown): T | null {
  return typeof value === "string" && (values as readonly string[]).includes(value) ? (value as T) : null;
}

export function isDefaultFilter(filter: InboxFilter): boolean {
  return (
    filter.source === DEFAULT_INBOX_FILTER.source &&
    filter.actor === DEFAULT_INBOX_FILTER.actor &&
    filter.triage === DEFAULT_INBOX_FILTER.triage
  );
}

export function sameFilter(a: InboxFilter, b: InboxFilter): boolean {
  return a.source === b.source && a.actor === b.actor && a.triage === b.triage;
}

function triageStates(triage: InboxTriage): ("active" | "snoozed" | "archived")[] {
  return triage === "all" ? ["active", "snoozed", "archived"] : [triage];
}

/** The `inbox.list` parameters for a filter. */
export function filterParams(filter: InboxFilter): Record<string, unknown> {
  return {
    sources: filter.source || undefined,
    actor: filter.actor === "any" ? undefined : filter.actor,
    triage_states: filter.triage === "active" ? undefined : triageStates(filter.triage),
  };
}

/** The stored `inbox.filter` object; `{}` for the default. */
export function serializeFilter(filter: InboxFilter): Record<string, unknown> {
  if (isDefaultFilter(filter)) return {};
  return {
    sources: filter.source ? [filter.source] : null,
    actor: filter.actor,
    triage_states: triageStates(filter.triage),
  };
}

export function parseFilter(raw: Record<string, unknown>): InboxFilter {
  const sources = Array.isArray(raw.sources) ? raw.sources : [];
  const states = Array.isArray(raw.triage_states) ? raw.triage_states.filter((s) => typeof s === "string") : [];
  const triage: InboxTriage =
    states.length === 0 || (states.length === 1 && states[0] === "active")
      ? "active"
      : states.length === 1
        ? (oneOf(INBOX_TRIAGE, states[0]) ?? "active")
        : "all";
  return {
    source: oneOf(INBOX_SOURCES, sources[0]) ?? "",
    actor: oneOf(INBOX_ACTORS, raw.actor) ?? "any",
    triage,
  };
}

/** An explicit filter in the URL (`?source=&actor=&state=`), or null when the
 *  URL names none — which is when the saved filter applies. */
export function filterFromQuery(search: string): InboxFilter | null {
  const query = new URLSearchParams(search);
  if (!query.has("source") && !query.has("actor") && !query.has("state")) return null;
  return {
    source: oneOf(INBOX_SOURCES, query.get("source")) ?? "",
    actor: oneOf(INBOX_ACTORS, query.get("actor")) ?? "any",
    triage: oneOf(INBOX_TRIAGE, query.get("state")) ?? "active",
  };
}

/** The `/inbox` path that states a filter explicitly. */
export function filterPath(filter: InboxFilter): string {
  const query = new URLSearchParams();
  if (filter.source) query.set("source", filter.source);
  if (filter.actor !== "any") query.set("actor", filter.actor);
  if (filter.triage !== "active") query.set("state", filter.triage);
  const qs = query.toString();
  return qs ? `/inbox?${qs}` : "/inbox";
}

/** A short human summary, for the active-filter chip and the badge label. */
export function describeFilter(filter: InboxFilter): string {
  const parts: string[] = [];
  if (filter.actor !== "any") parts.push(ACTOR_LABELS[filter.actor].toLowerCase());
  if (filter.source) parts.push(filter.source);
  if (filter.triage !== "active") parts.push(TRIAGE_LABELS[filter.triage].toLowerCase());
  return parts.join(" · ");
}

/** This account's saved Inbox filter. */
export const savedInboxFilter = accountPreference<InboxFilter>("inbox.filter", {
  empty: DEFAULT_INBOX_FILTER,
  parse: parseFilter,
  serialize: serializeFilter,
});

/** The reason recorded with a filter save. The person's own click on their
 *  own setting; the audit row says exactly what it changed. */
export function saveReason(filter: InboxFilter): string {
  return isDefaultFilter(filter)
    ? "Cleared the saved Inbox filter from the Inbox controls"
    : `Saved the Inbox filter (${describeFilter(filter)}) from the Inbox controls`;
}
