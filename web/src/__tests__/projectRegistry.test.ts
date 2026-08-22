import { describe, expect, it } from "vitest";

import { filterAndSortProjects, matchesProjectSearch } from "../projectRegistry";

const rows: Record<string, unknown>[] = [
  { slug: "vogt", name: "Vogt", lifecycle_state: "active", trust_state: "verified" },
  { slug: "cadastre", name: "Cadastre", lifecycle_state: "active", trust_state: "stale" },
  { slug: "old-thing", name: "Archived Thing", lifecycle_state: "archived", trust_state: "contested" },
  { slug: "nameless", name: "", lifecycle_state: "active" },
];

const row = (index: number): Record<string, unknown> => rows[index]!;

const slugs = (result: Record<string, unknown>[]) => result.map((row) => row["slug"]);

describe("matchesProjectSearch", () => {
  it("matches every row when the query is blank", () => {
    for (const row of rows) expect(matchesProjectSearch(row, "  ")).toBe(true);
  });

  it("matches on name, case-insensitively", () => {
    expect(matchesProjectSearch(row(0), "vo")).toBe(true);
    expect(matchesProjectSearch(row(0), "VOGT")).toBe(true);
  });

  it("matches on slug when the name does not", () => {
    expect(matchesProjectSearch(row(2), "old-thing")).toBe(true);
    expect(matchesProjectSearch(row(2), "zzz")).toBe(false);
  });
});

describe("filterAndSortProjects", () => {
  it("narrows to the rows whose name or slug matches", () => {
    expect(slugs(filterAndSortProjects(rows, "thing", "name"))).toEqual(["old-thing"]);
    expect(slugs(filterAndSortProjects(rows, "active", "name"))).toEqual([]);
  });

  it("sorts by display name, with the slug as the fallback name", () => {
    // "" name falls back to the slug "nameless"; ordering is by displayed text.
    expect(slugs(filterAndSortProjects(rows, "", "name"))).toEqual([
      "old-thing", // Archived Thing
      "cadastre", // Cadastre
      "nameless", // nameless (slug, name blank)
      "vogt", // Vogt
    ]);
  });

  it("sorts by lifecycle, breaking ties by name", () => {
    expect(slugs(filterAndSortProjects(rows, "", "lifecycle"))).toEqual([
      // active first: Cadastre, nameless, Vogt (by name), then archived
      "cadastre",
      "nameless",
      "vogt",
      "old-thing",
    ]);
  });

  it("sorts by trust, most-concerning first", () => {
    expect(slugs(filterAndSortProjects(rows, "", "trust"))).toEqual([
      "old-thing", // contested (rank 0)
      "cadastre", // stale (rank 1)
      "nameless", // unset -> unverified (rank 2)
      "vogt", // verified (rank 3)
    ]);
  });

  it("does not mutate the input array", () => {
    const before = slugs(rows);
    filterAndSortProjects(rows, "", "trust");
    expect(slugs(rows)).toEqual(before);
  });
});
