// The Projects registry list arrives in the server's order and, for a small
// estate, that is fine — but the list is where you go to find a project, so it
// wants a text filter (over name and slug) and a sort. Both are client-side:
// the server returns every registered project already (nothing is discovered),
// so narrowing and ordering are presentation, not another query.

export type ProjectSort = "name" | "lifecycle" | "trust";

type Row = Record<string, unknown>;

function field(row: Row, key: string): string {
  const value = row[key];
  return typeof value === "string" ? value : "";
}

/** The name shown on a card, falling back to the slug like the card does. */
function displayName(row: Row): string {
  return field(row, "name") || field(row, "slug");
}

/** Trust is never blank on screen; an unset state reads as `unverified`. */
function trust(row: Row): string {
  return field(row, "trust_state") || "unverified";
}

// Sort by trust groups the list by trust state, most-concerning first, so the
// projects that want attention rise to the top. Anything unrecognised sorts
// after the known states but before nothing.
const UNKNOWN_TRUST_RANK = 2;
const TRUST_RANK: Record<string, number> = {
  contested: 0,
  disputed: 0,
  stale: 1,
  unverified: UNKNOWN_TRUST_RANK,
  verified: 3,
};

function trustRank(row: Row): number {
  const rank = TRUST_RANK[trust(row)];
  // An unrecognised state sorts with `unverified` but just after it, so known
  // states keep their places and anything novel is not silently promoted.
  return rank === undefined ? UNKNOWN_TRUST_RANK + 0.5 : rank;
}

/** Case-insensitive substring match over a project's name and slug. */
export function matchesProjectSearch(row: Row, search: string): boolean {
  const needle = search.trim().toLowerCase();
  if (!needle) return true;
  return (
    field(row, "name").toLowerCase().includes(needle) ||
    field(row, "slug").toLowerCase().includes(needle)
  );
}

/**
 * Filter the registry by a text query, then order it. Every ordering breaks
 * ties by display name so the result is stable regardless of the input order.
 */
export function filterAndSortProjects(
  rows: readonly Row[],
  search: string,
  sort: ProjectSort,
): Row[] {
  const kept = rows.filter((row) => matchesProjectSearch(row, search));
  const byName = (a: Row, b: Row) =>
    displayName(a).localeCompare(displayName(b), undefined, { sensitivity: "base" });
  return [...kept].sort((a, b) => {
    if (sort === "lifecycle") {
      const compared = field(a, "lifecycle_state").localeCompare(
        field(b, "lifecycle_state"),
        undefined,
        { sensitivity: "base" },
      );
      return compared !== 0 ? compared : byName(a, b);
    }
    if (sort === "trust") {
      const compared = trustRank(a) - trustRank(b);
      return compared !== 0 ? compared : byName(a, b);
    }
    return byName(a, b);
  });
}
