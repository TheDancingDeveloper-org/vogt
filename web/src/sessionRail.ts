// The places rail's sections (WI-1736, docs/design/session-rail-2026-10-17.md).
//
// The rail used to be one flat list under a "Running" header, mostly finished
// work. This sorts the same sessions by what the operator has to do with them:
//
//   Needs you   a person must answer (approval, picker, a blocked report, input)
//               or a recent session errored
//   Running     working now
//   Ready/idle  alive and waiting at its prompt
//   Done recently  finished or asleep, last event within the retain window
//   Older       finished past the window; collapsed by default
//
// Oversight sessions are not pinned to the top: each is a parent row placed by
// its state, and the workers it started nest under it. A child that needs you
// is lifted into Needs you with a breadcrumb, so nesting never hides a request
// for attention. Pinned (bookmarked) sessions sit in their own strip above.
//
// Pure, so the grouping can be pinned by a unit test rather than through DOM.

import type { SessionSummary } from "./api";
import { isOversight, pickerWaiting } from "./sessionRowModel";

/** How long a finished or asleep session stays in "Done recently". */
export const RAIL_RECENT_MS = 6 * 60 * 60 * 1000;

export type RailSectionId = "needs" | "running" | "ready" | "done" | "older";

export const RAIL_SECTION_TITLES: Record<RailSectionId, string> = {
  needs: "Needs you",
  running: "Running",
  ready: "Ready / idle",
  done: "Done recently",
  older: "Older",
};

/** Section order on the rail, top to bottom. */
export const RAIL_SECTION_ORDER: readonly RailSectionId[] = ["needs", "running", "ready", "done", "older"];

/** What a row's children are doing, rolled up onto the parent row. */
export interface RailChildTally {
  total: number;
  running: number;
  needs: number;
  done: number;
}

export interface RailRow {
  session: SessionSummary;
  section: RailSectionId;
  /** Set on a child lifted into Needs you: the parent's name, for the breadcrumb. */
  lifted: string | null;
  /** The sessions nested under this one, in the order shown. Empty when collapsed or none. */
  children: SessionSummary[];
  /** Every nested child, whether shown under this row or lifted. Null when it has none. */
  tally: RailChildTally | null;
}

export interface RailSection {
  id: RailSectionId;
  title: string;
  count: number;
  rows: RailRow[];
}

export interface RailModel {
  pinned: SessionSummary[];
  /** How many pinned sessions need a person. They are counted here, not in a section. */
  pinnedNeeds: number;
  sections: RailSection[];
}

export interface RailOptions {
  now: number;
  /** Bookmarked session ids: shown in the Pinned strip, not in a section. */
  bookmarked?: ReadonlySet<string>;
  /** Child engine id → the engine id of the session that started it (Vogt's
   *  `started_by_session`). Empty until Vogt says, and then nothing nests. */
  starters?: ReadonlyMap<string, string>;
  /** Parent ids whose children are folded away. */
  collapsed?: ReadonlySet<string>;
  /** Free-text filter over name, work item, cwd and template. */
  query?: string;
  /** The engine's `hidden_count`: how many finished sessions are past the
   *  retain window. Older's header shows this, so it reads right before any
   *  older row is loaded (WI-1736). */
  hiddenCount?: number;
  /** The order the previous render showed, so a held list does not reshuffle
   *  while a row's menu is open (#686). Ids not in it keep their place after. */
  held?: readonly string[];
}

/** When a session last did anything, in ms. Unparseable is treated as old. */
export function railLastEventMs(s: SessionSummary): number {
  const at = s.last_event_at ?? s.activity_changed_at ?? s.created_at;
  const ms = Date.parse(at);
  return Number.isNaN(ms) ? 0 : ms;
}

/** Which section a session belongs in on its own, ignoring nesting. */
export function railSectionOf(s: SessionSummary, now: number): RailSectionId {
  const recent = now - railLastEventMs(s) < RAIL_RECENT_MS;
  if (s.activity === "hibernated") return recent ? "done" : "older";
  if (s.activity === "stopped") return recent ? "done" : "older";
  if (s.exit_code !== null) {
    if (!recent) return "older";
    return s.exit_code === 0 ? "done" : "needs";
  }
  if (s.activity === "exited") return recent ? "done" : "older";
  if (s.activity === "errored") return recent ? "needs" : "older";
  if (s.blocked || s.activity === "awaiting-approval" || s.activity === "waiting-for-input") {
    return "needs";
  }
  if (s.activity === "running") return "running";
  return "ready";
}

/** Whether a section means the session is still doing something for someone. */
function isLiveSection(section: RailSectionId): boolean {
  return section === "needs" || section === "running" || section === "ready";
}

function matches(s: SessionSummary, query: string): boolean {
  if (!query) return true;
  return [s.name, s.work_item, s.cwd, s.template]
    .some((field) => (field ?? "").toLowerCase().includes(query));
}

/** Within a section: a waiting session sorts oldest-wait first, since the
 *  longest wait is the most urgent; everything else most recent first. */
function compareInSection(section: RailSectionId, a: SessionSummary, b: SessionSummary): number {
  if (section === "needs") {
    return sinceMs(a) - sinceMs(b);
  }
  return railLastEventMs(b) - railLastEventMs(a);
}

function sinceMs(s: SessionSummary): number {
  const ms = Date.parse(s.activity_changed_at || s.created_at);
  return Number.isNaN(ms) ? 0 : ms;
}

/** Build the rail: the pinned strip and the sections, with nesting and lift-up.
 *  Pure. The result is a fresh model each call. */
export function buildRailModel(sessions: readonly SessionSummary[], options: RailOptions): RailModel {
  const now = options.now;
  const bookmarked = options.bookmarked ?? new Set<string>();
  const starters = options.starters ?? new Map<string, string>();
  const collapsed = options.collapsed ?? new Set<string>();
  const query = (options.query ?? "").trim().toLowerCase();
  const byId = new Map(sessions.map((s) => [s.id, s]));

  const pinned = sessions.filter((s) => bookmarked.has(s.id) && matches(s, query));

  // The starter of a child, when that starter is a listed oversight session
  // that is not itself pinned. Nesting is one level deep: oversight → workers.
  const parentOf = new Map<string, SessionSummary>();
  for (const s of sessions) {
    if (bookmarked.has(s.id)) continue;
    const starterId = starters.get(s.id);
    if (!starterId || starterId === s.id) continue;
    const starter = byId.get(starterId);
    if (!starter || bookmarked.has(starter.id) || !isOversight(starter)) continue;
    parentOf.set(s.id, starter);
  }

  // One level only: a session that is itself nested does not take children,
  // or its children would have no row to hang under.
  for (const [childId, parent] of parentOf) {
    if (parentOf.has(parent.id)) parentOf.delete(childId);
  }

  const childrenOf = new Map<string, SessionSummary[]>();
  for (const [childId, parent] of parentOf) {
    const child = byId.get(childId);
    if (!child) continue;
    const list = childrenOf.get(parent.id) ?? [];
    list.push(child);
    childrenOf.set(parent.id, list);
  }

  // A nested child's own section: a child that needs you is lifted out.
  const sectionOfId = new Map<string, RailSectionId>();
  for (const s of sessions) {
    if (bookmarked.has(s.id)) continue;
    sectionOfId.set(s.id, railSectionOf(s, now));
  }

  // A parent row's section is its own, except that a finished parent with
  // live children is not Older: it sits where its children are.
  const parentSection = (parent: SessionSummary): RailSectionId => {
    const own = sectionOfId.get(parent.id) ?? "older";
    if (own !== "done" && own !== "older") return own;
    const kids = childrenOf.get(parent.id) ?? [];
    if (kids.some((kid) => sectionOfId.get(kid.id) === "running")) return "running";
    if (kids.some((kid) => isLiveSection(sectionOfId.get(kid.id) ?? "older"))) return "ready";
    return own;
  };

  const sectionRows = new Map<RailSectionId, RailRow[]>(RAIL_SECTION_ORDER.map((id) => [id, []]));
  const placedAsChild = new Set<string>();
  for (const s of sessions) {
    if (bookmarked.has(s.id)) continue;
    const parent = parentOf.get(s.id);
    if (!parent) continue;
    // Children of a parent that is not shown are placed by their own state.
    if (!matches(s, query) && !matches(parent, query)) continue;
    const section = sectionOfId.get(s.id) ?? "older";
    if (section === "needs") {
      // Lifted: a child that needs you is shown in Needs you, with a breadcrumb.
      sectionRows.get("needs")!.push({ session: s, section, lifted: parent.name, children: [], tally: null });
      placedAsChild.add(s.id);
    }
  }

  for (const s of sessions) {
    if (bookmarked.has(s.id) || placedAsChild.has(s.id)) continue;
    if (parentOf.has(s.id)) continue; // shown under its parent below
    const section = isOversight(s) || childrenOf.has(s.id) ? parentSection(s) : (sectionOfId.get(s.id) ?? "older");
    if (!matches(s, query)) {
      const anyChild = (childrenOf.get(s.id) ?? []).some((kid) => matches(kid, query));
      if (!anyChild) continue;
    }
    const kids = childrenOf.get(s.id) ?? [];
    const shownKids = kids
      .filter((kid) => !placedAsChild.has(kid.id))
      .filter((kid) => matches(s, query) || matches(kid, query));
    const tally: RailChildTally | null = kids.length
      ? {
          total: kids.length,
          running: kids.filter((kid) => sectionOfId.get(kid.id) === "running").length,
          needs: kids.filter((kid) => sectionOfId.get(kid.id) === "needs").length,
          done: kids.filter((kid) => sectionOfId.get(kid.id) === "done" || sectionOfId.get(kid.id) === "older").length,
        }
      : null;
    sectionRows.get(section)!.push({
      session: s,
      section,
      lifted: null,
      children: collapsed.has(s.id) ? [] : shownKids,
      tally,
    });
  }

  const held = options.held ? new Map(options.held.map((id, index) => [id, index])) : null;
  const pinnedNeeds = pinned.filter((s) => railSectionOf(s, now) === "needs").length;
  const sections: RailSection[] = RAIL_SECTION_ORDER.map((id) => {
    const rows = sectionRows.get(id)!;
    rows.sort((a, b) => {
      if (held) {
        const ai = held.get(a.session.id) ?? Number.MAX_SAFE_INTEGER;
        const bi = held.get(b.session.id) ?? Number.MAX_SAFE_INTEGER;
        if (ai !== bi) return ai - bi;
      }
      return compareInSection(id, a.session, b.session);
    });
    // Older counts what the engine hid, loaded or not.
    const count = id === "older" && options.hiddenCount !== undefined ? options.hiddenCount : rows.length;
    return { id, title: RAIL_SECTION_TITLES[id], count, rows };
  });

  return { pinned, pinnedNeeds, sections };
}

/** Finished or asleep: nothing to do until someone wakes or restarts it. */
export function railFinished(s: Pick<SessionSummary, "activity" | "exit_code">): boolean {
  return s.exit_code !== null || s.activity === "hibernated" || s.activity === "stopped" || s.activity === "exited";
}

/** A section header row. Its `id` is the section, or `pinned` for the strip. */
export interface RailHeadEntry {
  kind: "head";
  key: string;
  id: RailSectionId | "pinned";
  title: string;
  count: number;
  open: boolean;
  /** Pinned strip only: how many of its sessions need a person. */
  needs: number;
}

/** A session row: a top-level row, a nested child, or a pinned one. */
export interface RailRowEntry {
  kind: "row";
  key: string;
  session: SessionSummary;
  section: RailSectionId | "pinned";
  depth: 0 | 1;
  /** A child lifted into Needs you: its parent's name, for the breadcrumb. */
  lifted: string | null;
  /** Roll-up of a parent's children; null when it has none. */
  tally: RailChildTally | null;
  /** Whether a parent's children are shown; null for a row with none. */
  expanded: boolean | null;
}

export type RailEntry = RailHeadEntry | RailRowEntry;

/** Flatten the model into the one list the rail renders, with section
 *  headers in between. Headers of empty sections are left out. A collapsed
 *  section keeps its header and drops its rows.
 *
 *  `previous` is the last list shown. An entry that reads the same keeps its
 *  object, so a refresh does not replace a row that holds focus (WI-1682). */
export function flattenRail(
  model: RailModel,
  options: {
    /** Whether a section, or the pinned strip (`pinned`), is open. */
    open: (id: RailSectionId | "pinned") => boolean;
    /** Whether a parent's children are shown. */
    expanded: (id: string) => boolean;
  },
  previous: readonly RailEntry[] = [],
): RailEntry[] {
  const prior = new Map(previous.map((entry) => [entry.key, entry]));
  const out: RailEntry[] = [];
  const add = (entry: RailEntry) => {
    const old = prior.get(entry.key);
    out.push(old && sameEntry(old, entry) ? old : entry);
  };

  const pushRows = (rows: readonly RailRow[], section: RailSectionId | "pinned") => {
    for (const row of rows) {
      const shown = options.expanded(row.session.id);
      add({
        kind: "row",
        key: `row:${row.session.id}`,
        session: row.session,
        section,
        depth: 0,
        lifted: row.lifted,
        tally: row.tally,
        expanded: row.tally ? shown : null,
      });
      for (const child of row.children) {
        add({
          kind: "row",
          key: `row:${row.session.id}/${child.id}`,
          session: child,
          section,
          depth: 1,
          lifted: null,
          tally: null,
          expanded: null,
        });
      }
    }
  };

  if (model.pinned.length > 0) {
    add({
      kind: "head",
      key: "head:pinned",
      id: "pinned",
      title: "Pinned",
      count: model.pinned.length,
      open: options.open("pinned"),
      needs: model.pinnedNeeds,
    });
    if (options.open("pinned")) {
      for (const s of model.pinned) {
        add({ kind: "row", key: `pin:${s.id}`, session: s, section: "pinned", depth: 0, lifted: null, tally: null, expanded: null });
      }
    }
  }

  for (const section of model.sections) {
    if (section.count === 0) continue;
    const open = options.open(section.id);
    add({ kind: "head", key: `head:${section.id}`, id: section.id, title: section.title, count: section.count, open, needs: 0 });
    if (open) pushRows(section.rows, section.id);
  }
  return out;
}

function sameEntry(a: RailEntry, b: RailEntry): boolean {
  if (a.kind === "head") {
    // `open` is not compared: a header reads its open state live, so a fold
    // must not replace the header (and drop the focus on it).
    return b.kind === "head" && a.id === b.id && a.title === b.title && a.count === b.count && a.needs === b.needs;
  }
  if (b.kind === "head") return false;
  return a.session === b.session
    && a.section === b.section
    && a.depth === b.depth
    && a.lifted === b.lifted
    && a.expanded === b.expanded
    && JSON.stringify(a.tally) === JSON.stringify(b.tally);
}

/** A small glyph and the words it stands for. The words are the tooltip and
 *  the accessible name, so colour and glyph are never the only signal. */
export interface RailBadge {
  glyph: string;
  label: string;
}

/** The state badges a row shows (design §4): `?` a picker waiting for a
 *  choice, `⚠` an approval, `⛔` blocked, `✗` errored, `AP` autopilot,
 *  `☾` hibernated, `✓` ready (idle at its prompt). */
export function railBadges(s: SessionSummary, now: number): RailBadge[] {
  const badges: RailBadge[] = [];
  if (pickerWaiting(s)) badges.push({ glyph: "?", label: "waiting for a choice" });
  else if (s.exit_code === null && s.activity === "awaiting-approval") badges.push({ glyph: "⚠", label: "needs approval" });
  if (s.blocked && s.exit_code === null) badges.push({ glyph: "⛔", label: "blocked on you" });
  if (s.activity === "errored" || (s.exit_code !== null && s.exit_code !== 0 && s.activity !== "stopped")) {
    badges.push({ glyph: "✗", label: "errored" });
  }
  if (s.autopilot) badges.push({ glyph: "AP", label: "autopilot" });
  if (s.activity === "hibernated") badges.push({ glyph: "☾", label: "hibernated" });
  if (railSectionOf(s, now) === "ready") badges.push({ glyph: "✓", label: "ready" });
  return badges;
}
