/** Ranking for the command palette's main list.
 *
 * The old filter was an unscored subsequence over the label *or* the
 * description, kept in source order — so `inbox` could surface a work item
 * whose body happened to contain the letters before it surfaced "Open Inbox",
 * and `needs` never floated the session literally named `needs-attention`
 * above everything else. This scores a match instead, and a label match always
 * outranks a description-only one, so the row whose *name* you typed wins.
 *
 * The tiers, best to worst: an exact label, a label prefix, a query that
 * starts a word inside the label, a contiguous substring of the label, a
 * subsequence of the label, then — only if the label did not match at all — the
 * same ladder against the description. `rankCommands` keeps ties in their
 * original order, so a caller that wants sessions ahead of work items on an
 * equal score just lists sessions first.
 */

export interface Scorable {
  label: string;
  description?: string;
}

// Label matches occupy a band far above any description match, so "label beats
// description" holds even when the label match is the weakest kind (a
// subsequence) and the description match is the strongest (exact).
export const TIER_EXACT = 6;
export const TIER_PREFIX = 5;
export const TIER_WORD_START = 4;
export const TIER_SUBSTRING = 3;
export const TIER_SUBSEQUENCE = 2;

const LABEL_BASE = 100;

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function isSubsequence(pattern: string, text: string): boolean {
  let pi = 0;
  for (let ti = 0; ti < text.length && pi < pattern.length; ti++) {
    if (text[ti] === pattern[pi]) pi++;
  }
  return pi === pattern.length;
}

/**
 * How well `pattern` matches `text`, as one of the tier constants, or 0 for no
 * match. Case-insensitive. Empty pattern is not a match (the caller shows the
 * unfiltered list for an empty query itself).
 */
export function textMatchTier(pattern: string, text: string): number {
  const p = pattern.toLowerCase().trim();
  const t = text.toLowerCase();
  if (!p) return 0;
  if (t === p) return TIER_EXACT;
  if (t.startsWith(p)) return TIER_PREFIX;
  // A word-start is the query sitting at the front of a word inside the label —
  // after a space, a dash, or any non-alphanumeric seam. The `^` case is a
  // prefix and already returned above; this catches the interior ones.
  const wordStart = new RegExp(`[^a-z0-9]${escapeRegExp(p)}`);
  if (wordStart.test(t)) return TIER_WORD_START;
  if (t.includes(p)) return TIER_SUBSTRING;
  if (isSubsequence(p, t)) return TIER_SUBSEQUENCE;
  return 0;
}

/**
 * A command's score for `query`: a positive number when it matches, 0 when it
 * does not. A label match lands in the 100+ band; a description-only match in
 * the 1-6 band; so any label match outranks any description-only match.
 */
export function scoreCommand(query: string, command: Scorable): number {
  const labelTier = textMatchTier(query, command.label);
  if (labelTier > 0) return LABEL_BASE + labelTier;
  const descTier = command.description ? textMatchTier(query, command.description) : 0;
  return descTier;
}

/**
 * Keep the commands that match `query`, best score first. Stable: equal scores
 * stay in the order they were passed, so a caller orders its own ties (e.g.
 * sessions before work items) by the order it builds the list.
 */
export function rankCommands<T extends Scorable>(query: string, commands: readonly T[]): T[] {
  const q = query.trim();
  if (!q) return [...commands];
  return commands
    .map((command, index) => ({ command, index, score: scoreCommand(q, command) }))
    .filter((entry) => entry.score > 0)
    .sort((a, b) => b.score - a.score || a.index - b.index)
    .map((entry) => entry.command);
}
