// Naming rules for terminal sessions and split panes.
//
// New sessions are created immediately (no name prompt) unless the user asks
// for one by holding Shift on the create action. The default name is the
// basename of the session's cwd, deduped against the names already in use so
// two shells opened in the same directory read as `vogt` and `vogt-2` rather
// than colliding. Splits derived from a session hang off its base name with a
// `▸N` marker (`vogt ▸2`), so a split reads as a child of what it came from.

const FALLBACK_BASE = "shell";

/**
 * The basename of a cwd, used as the default session name. Trailing slashes
 * and an empty/undefined cwd fall back to a stable placeholder so a name is
 * always produced.
 */
export function baseNameFromCwd(cwd: string | null | undefined): string {
  if (!cwd) return FALLBACK_BASE;
  const segments = cwd.split(/[/\\]/).filter((part) => part.trim().length > 0);
  const last = segments[segments.length - 1];
  return last && last.trim().length > 0 ? last : FALLBACK_BASE;
}

/**
 * `base`, or the first free `base-N` (N starting at 2) when `base` — or a
 * lower N — is already taken. Dedupe keeps same-directory shells distinct.
 */
export function uniqueSessionName(
  base: string,
  existingNames: Iterable<string>,
): string {
  const taken = new Set(existingNames);
  if (!taken.has(base)) return base;
  let n = 2;
  while (taken.has(`${base}-${n}`)) n += 1;
  return `${base}-${n}`;
}

/**
 * The next free `base ▸N` (N starting at 2) — the name for a pane split off a
 * session whose own base is `base`.
 */
export function uniqueSplitName(
  base: string,
  existingNames: Iterable<string>,
): string {
  const taken = new Set(existingNames);
  let n = 2;
  while (taken.has(`${base} ▸${n}`)) n += 1;
  return `${base} ▸${n}`;
}

/**
 * The auto-name for a brand-new session opened at `cwd`, deduped against the
 * names already in use.
 */
export function autoSessionName(
  cwd: string | null | undefined,
  existingNames: Iterable<string>,
): string {
  return uniqueSessionName(baseNameFromCwd(cwd), existingNames);
}

/**
 * The auto-name for a pane split off a session at `sourceCwd`, deduped so a
 * second split reads as `vogt ▸3`.
 */
export function autoSplitName(
  sourceCwd: string | null | undefined,
  existingNames: Iterable<string>,
): string {
  return uniqueSplitName(baseNameFromCwd(sourceCwd), existingNames);
}
