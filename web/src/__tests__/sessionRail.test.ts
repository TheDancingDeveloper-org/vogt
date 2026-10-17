// The places rail's sections (sessionRail.ts, WI-1736): which section a session
// is in, oversight nesting and lift-up, pinning, search, counts, and the order
// a held list keeps. The starters map (railStarters.ts) is pinned here too.

import { describe, expect, it } from "vitest";
import type { SessionSummary } from "../api";
import type { SessionSummary as VogtSessionSummary } from "../vogtApi";
import {
  RAIL_RECENT_MS,
  buildRailModel,
  flattenRail,
  railSectionOf,
} from "../sessionRail";
import { startersById } from "../railStarters";
import { pickerWaiting, sessionStateWord } from "../sessionRowModel";

const NOW = Date.parse("2026-10-17T12:00:00Z");
const minutesAgo = (m: number) => new Date(NOW - m * 60_000).toISOString();

function session(over: Partial<SessionSummary> & { id: string }): SessionSummary {
  return {
    ...over,
    name: over.name ?? over.id,
    activity: over.activity ?? "idle",
    exit_code: over.exit_code ?? null,
    scrollback_bytes: 0,
    cwd: over.cwd ?? "/workspace",
    created_at: over.created_at ?? minutesAgo(5),
    last_event_at: over.last_event_at ?? minutesAgo(5),
  };
}

const ids = (rows: { session: SessionSummary }[]) => rows.map((row) => row.session.id);

describe("railSectionOf", () => {
  it("puts a person-blocking wait in Needs you, whatever its age", () => {
    expect(railSectionOf(session({ id: "a", activity: "awaiting-approval" }), NOW)).toBe("needs");
    expect(railSectionOf(session({ id: "b", activity: "waiting-for-input" }), NOW)).toBe("needs");
    expect(railSectionOf(session({ id: "c", blocked: { reason: "x", at: minutesAgo(1) } as never }), NOW)).toBe("needs");
  });

  it("splits live work into Running and Ready / idle", () => {
    expect(railSectionOf(session({ id: "r", activity: "running" }), NOW)).toBe("running");
    expect(railSectionOf(session({ id: "i", activity: "idle" }), NOW)).toBe("ready");
  });

  it("keeps finished or asleep work in Done recently inside the retain window", () => {
    expect(railSectionOf(session({ id: "h", activity: "hibernated", last_event_at: minutesAgo(30) }), NOW)).toBe("done");
    expect(railSectionOf(session({ id: "e", activity: "exited", exit_code: 0, last_event_at: minutesAgo(30) }), NOW)).toBe("done");
  });

  it("sends finished work past six hours to Older", () => {
    const old = new Date(NOW - RAIL_RECENT_MS - 60_000).toISOString();
    expect(railSectionOf(session({ id: "o", activity: "hibernated", last_event_at: old }), NOW)).toBe("older");
    expect(railSectionOf(session({ id: "x", exit_code: 0, activity: "exited", last_event_at: old }), NOW)).toBe("older");
  });

  it("treats a recent non-zero exit as needing you, and a stopped session as done", () => {
    expect(railSectionOf(session({ id: "f", exit_code: 1, activity: "errored" }), NOW)).toBe("needs");
    expect(railSectionOf(session({ id: "s", exit_code: 137, activity: "stopped" }), NOW)).toBe("done");
  });
});

describe("buildRailModel", () => {
  it("lists each section in order with its count, and drops nothing", () => {
    const model = buildRailModel([
      session({ id: "n", activity: "awaiting-approval" }),
      session({ id: "r", activity: "running" }),
      session({ id: "i", activity: "idle" }),
      session({ id: "d", activity: "hibernated", last_event_at: minutesAgo(60) }),
      session({ id: "o", activity: "hibernated", last_event_at: new Date(NOW - RAIL_RECENT_MS - 1000).toISOString() }),
    ], { now: NOW });
    expect(model.sections.map((section) => [section.id, section.count])).toEqual([
      ["needs", 1],
      ["running", 1],
      ["ready", 1],
      ["done", 1],
      ["older", 1],
    ]);
  });

  it("puts a pinned session in the strip, not in a section", () => {
    const model = buildRailModel([
      session({ id: "p", activity: "running" }),
      session({ id: "q", activity: "running" }),
    ], { now: NOW, bookmarked: new Set(["p"]) });
    expect(model.pinned.map((s) => s.id)).toEqual(["p"]);
    expect(model.sections.flatMap((section) => ids(section.rows))).toEqual(["q"]);
  });

  it("nests workers under their oversight session and rolls their states up", () => {
    const starters = new Map([["w1", "ovs"], ["w2", "ovs"], ["w3", "ovs"]]);
    const model = buildRailModel([
      session({ id: "ovs", role: "oversight", activity: "idle" }),
      session({ id: "w1", activity: "running" }),
      session({ id: "w2", activity: "idle" }),
      session({ id: "w3", activity: "exited", exit_code: 0 }),
    ], { now: NOW, starters });
    const parent = model.sections.flatMap((section) => section.rows).find((row) => row.session.id === "ovs")!;
    expect(parent.children.map((s) => s.id)).toEqual(["w1", "w2", "w3"]);
    expect(parent?.tally).toEqual({ total: 3, running: 1, needs: 0, done: 1 });
  });

  it("does not pin oversight to the top: a finished oversight sits by its state", () => {
    const model = buildRailModel([
      session({ id: "live", activity: "running" }),
      session({ id: "ovs", role: "oversight", activity: "hibernated", last_event_at: minutesAgo(10) }),
    ], { now: NOW });
    const shown = model.sections.filter((section) => section.count > 0);
    expect(shown.map((section) => [section.id, ids(section.rows)])).toEqual([
      ["running", ["live"]],
      ["done", ["ovs"]],
    ]);
  });

  it("does not put a finished oversight with live children in Older", () => {
    const old = new Date(NOW - RAIL_RECENT_MS - 60_000).toISOString();
    const model = buildRailModel([
      session({ id: "ovs", role: "oversight", activity: "hibernated", last_event_at: old }),
      session({ id: "w", activity: "running" }),
    ], { now: NOW, starters: new Map([["w", "ovs"]]) });
    const home = model.sections.find((section) => ids(section.rows).includes("ovs"));
    expect(home?.id).toBe("running");
  });

  it("lifts a child that needs you into Needs you, with its parent as the breadcrumb", () => {
    const model = buildRailModel([
      session({ id: "ovs", role: "oversight", activity: "running" }),
      session({ id: "w", activity: "awaiting-approval" }),
      session({ id: "v", activity: "running" }),
    ], { now: NOW, starters: new Map([["w", "ovs"], ["v", "ovs"]]) });
    const needs = model.sections.find((section) => section.id === "needs");
    expect(needs?.rows).toEqual([expect.objectContaining({ lifted: "ovs" })]);
    const parent = model.sections.flatMap((section) => section.rows).find((row) => row.session.id === "ovs")!;
    // Shown under the parent, the lifted child is not repeated.
    expect(parent?.children.map((s) => s.id)).toEqual(["v"]);
    expect(parent.tally).toEqual({ total: 2, running: 1, needs: 1, done: 0 });
  });

  it("nests one level only: a worker of a worker is a plain row", () => {
    const model = buildRailModel([
      session({ id: "ovs", role: "oversight", activity: "running" }),
      session({ id: "mid", activity: "running" }),
      session({ id: "leaf", activity: "running" }),
    ], { now: NOW, starters: new Map([["mid", "ovs"], ["leaf", "mid"]]) });
    const all = model.sections.flatMap((section) => section.rows);
    expect(all.find((row) => row.session.id === "ovs")?.children.map((s) => s.id)).toEqual(["mid"]);
    expect(all.some((row) => row.session.id === "leaf")).toBe(true);
  });

  it("leaves a child flat when its starter is pinned or not listed", () => {
    const model = buildRailModel([
      session({ id: "ovs", role: "oversight", activity: "running" }),
      session({ id: "w", activity: "running" }),
    ], { now: NOW, starters: new Map([["w", "ovs"]]), bookmarked: new Set(["ovs"]) });
    expect(model.pinned.map((s) => s.id)).toEqual(["ovs"]);
    expect(ids(model.sections.find((section) => section.id === "running")!.rows)).toEqual(["w"]);
  });

  it("folds a parent's children away when it is collapsed, keeping the tally", () => {
    const model = buildRailModel([
      session({ id: "ovs", role: "oversight", activity: "running" }),
      session({ id: "w", activity: "running" }),
    ], { now: NOW, starters: new Map([["w", "ovs"]]), collapsed: new Set(["ovs"]) });
    const parent = model.sections[1]!.rows[0]!;
    expect(parent.children).toEqual([]);
    expect(parent.tally?.total).toBe(1);
  });

  it("filters by name, work item, cwd and template, keeping a parent whose child matches", () => {
    const starters = new Map([["w", "ovs"]]);
    const sessions = [
      session({ id: "ovs", name: "ovs-main", role: "oversight", activity: "running" }),
      session({ id: "w", name: "fix-login", work_item: "WI-42", activity: "running" }),
      session({ id: "other", name: "scratch", cwd: "/home/x/notes", activity: "running" }),
    ];
    expect(ids(buildRailModel(sessions, { now: NOW, query: "wi-42", starters }).sections[1]!.rows)).toEqual(["ovs"]);
    expect(ids(buildRailModel(sessions, { now: NOW, query: "notes", starters }).sections[1]!.rows)).toEqual(["other"]);
    expect(buildRailModel(sessions, { now: NOW, query: "zzz", starters }).sections).toEqual(
      expect.arrayContaining([expect.objectContaining({ count: 0 })]),
    );
  });

  it("keeps a held order while a menu is open, even when a section would reorder", () => {
    const sessions = [
      session({ id: "a", activity: "running", last_event_at: minutesAgo(1) }),
      session({ id: "b", activity: "running", last_event_at: minutesAgo(9) }),
    ];
    const held = buildRailModel(sessions, { now: NOW, held: ["b", "a"] });
    expect(ids(held.sections[1]!.rows)).toEqual(["b", "a"]);
  });
});

describe("flattenRail", () => {
  it("drops the header of an empty section and keeps the rows of an open one", () => {
    const model = buildRailModel([session({ id: "r", activity: "running" })], { now: NOW });
    const entries = flattenRail(model, { open: () => true, expanded: () => true });
    expect(entries.map((e) => (e.kind === "head" ? `head:${e.id}` : `row:${e.session.id}`))).toEqual([
      "head:running",
      "row:r",
    ]);
  });

  it("folds a collapsed section to its header", () => {
    const model = buildRailModel([session({ id: "r", activity: "running" })], { now: NOW });
    const entries = flattenRail(model, { open: (id) => id !== "running", expanded: () => true });
    expect(entries).toHaveLength(1);
    expect(entries[0]).toMatchObject({ kind: "head", id: "running", open: false });
  });

  it("reuses the previous entry object when nothing it shows has changed", () => {
    const sessions = [session({ id: "r", activity: "running" })];
    const first = flattenRail(buildRailModel(sessions, { now: NOW }), { open: () => true, expanded: () => true });
    const again = flattenRail(
      buildRailModel(sessions, { now: NOW }),
      { open: () => true, expanded: () => true },
      first,
    );
    expect(again[1]).toBe(first[1]);
  });
});

describe("startersById", () => {
  it("maps each linked engine session to the session that started it", () => {
    const rows = [
      { id: "ses_1", engine_session_id: "w1", started_by_session: "ovs" },
      { id: "ses_2", engine_session_id: "ovs", started_by_session: null },
      { id: "ses_3", engine_session_id: "", started_by_session: "ovs" },
    ] as unknown as VogtSessionSummary[];
    expect(Object.fromEntries(startersById(rows))).toEqual({ w1: "ovs" });
  });
});

describe("picker waits", () => {
  it("reads a picker as waiting for a choice, not as an approval", () => {
    const picker = session({
      id: "p",
      activity: "awaiting-approval",
      approval: { question: "Pick", command_excerpt: "", detected_at: minutesAgo(1), kind: "picker" },
    });
    expect(pickerWaiting(picker)).toBe(true);
    expect(sessionStateWord(picker, NOW)).toMatch(/^waiting for a choice/);
    const approval = { ...picker, approval: { ...picker.approval!, kind: "approval" as const } };
    expect(pickerWaiting(approval)).toBe(false);
    expect(sessionStateWord(approval, NOW)).toMatch(/^needs approval/);
  });
});
