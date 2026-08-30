// The session row, as data rather than markup.
//
// One place lists running sessions three times over: the desktop Places rail
// (App.tsx), the desktop Sessions overview and the phone Sessions body (both
// through `SessionList`). These helpers are the words a row uses for a
// session's activity and the order a set of them is shown in, kept here so the
// rail and the list cannot drift apart — a session that reads "running · 6m"
// in one and "running" in the other is the same bug the shared breakpoint in
// `narrow.ts` exists to prevent for widths.

import type { ActivityState, SessionSummary } from "./api";
import { formatAgo } from "./viewAge";

/** The activity-dot class for a session, folding a finished session's exit
 *  code into `done`/`errored` rather than leaving it on the live-activity
 *  vocabulary. */
export function activityClass(s: SessionSummary): string {
  if (s.exit_code !== null) {
    return s.exit_code === 0 ? "done" : "errored";
  }
  return s.activity;
}

/** The session's activity as a word, exit code first so a finished session
 *  reads as finished rather than as whatever it was doing when it stopped. */
export function activityLabel(s: ActivityState, exit: number | null): string {
  if (exit !== null) return exit === 0 ? "exited (0)" : `errored (${exit})`;
  switch (s) {
    case "waiting-for-input":
      return "waiting for input";
    default:
      return s;
  }
}

/** How long a session has held its current activity, from the one timestamp
 *  the engine actually reports. Absent on an older engine — omitted rather
 *  than guessed. */
export function sessionActivityAge(s: SessionSummary, now: number): string | null {
  if (!s.activity_changed_at) return null;
  const changed = Date.parse(s.activity_changed_at);
  if (Number.isNaN(changed)) return null;
  return formatAgo(now - changed);
}

/** The row's state word beside the dot: "waiting for input · 40s",
 *  "running · 6m". Colour is never the only signal, so this line exists
 *  whether or not the age is known. */
export function sessionStateWord(
  s: SessionSummary,
  now: number,
  staleAt?: string | null,
): string {
  const label = activityLabel(s.activity, s.exit_code);
  if (staleAt) {
    const stamp = new Date(staleAt);
    if (!Number.isNaN(stamp.getTime())) {
      return `${label} · as of ${stamp.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`;
    }
  }
  const age = sessionActivityAge(s, now);
  return age ? `${label} · ${age}` : label;
}

const ATTENTION_ORDER: Record<string, number> = {
  "waiting-for-input": 0,
  errored: 1,
  running: 2,
  idle: 3,
  exited: 4,
};

/** Lower ranks want a reader's attention first. A non-zero exit is an error
 *  regardless of the last live activity; a clean exit sinks to the bottom. */
export function attentionRank(session: SessionSummary): number {
  if (session.exit_code !== null) return session.exit_code === 0 ? 4 : 1;
  return ATTENTION_ORDER[session.activity] ?? 3;
}

/** Attention first, then most-recently-active. Pure and total — a copy, so the
 *  caller's array is left alone — which is what lets a unit test pin it. */
export function sortSessionsByAttention(sessions: SessionSummary[]): SessionSummary[] {
  return [...sessions].sort((left, right) => {
    const attentionDelta = attentionRank(left) - attentionRank(right);
    if (attentionDelta !== 0) return attentionDelta;
    return Date.parse(right.activity_changed_at || right.created_at)
      - Date.parse(left.activity_changed_at || left.created_at);
  });
}

/** Attention order plus one stable bookmark partition for the Places rail. */
export function sortSessionsForRail(
  sessions: SessionSummary[],
  bookmarked: ReadonlySet<string>,
): SessionSummary[] {
  return sortSessionsByAttention(sessions).sort(
    (left, right) => Number(bookmarked.has(right.id)) - Number(bookmarked.has(left.id)),
  );
}
