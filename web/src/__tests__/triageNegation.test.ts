// Negation (#351): the exclusion set is encoded in the URL, round-trips
// through `filtersFromQuery`/`queryFromSearch`, and — the load-bearing part —
// is carried under its own `not_*` keys so a build that predates it drops the
// token rather than misreading it as an inclusion.

import { describe, expect, it } from "vitest";
import {
  encodeFilters,
  excludeCount,
  filtersFromQuery,
  queryFromSearch,
  describeFilters,
} from "../Board";

describe("board negation encoding (#351)", () => {
  it("decodes each facet's exclusions from its own not_ key", () => {
    const filters = filtersFromQuery(
      queryFromSearch(
        "project=core&not_project=vogt&not_kind=bug&not_kind=chore&not_state=done&not_label=infra",
      ),
    );
    // Inclusion and exclusion are independent axes.
    expect(filters.project).toBe("core");
    expect(filters.exclude.projects).toEqual(["vogt"]);
    expect(filters.exclude.kinds).toEqual(["bug", "chore"]);
    expect(filters.exclude.states).toEqual(["done"]);
    expect(filters.exclude.labels).toEqual(["infra"]);
    expect(excludeCount(filters.exclude)).toBe(5);
  });

  it("round-trips an exclusion set through encode → decode unchanged", () => {
    const original = filtersFromQuery(
      queryFromSearch("not_project=vogt&not_assignee=local:ana&not_initiative=INIT-1&kind=feature"),
    );
    const roundTripped = filtersFromQuery(queryFromSearch(encodeFilters(original)));
    expect(roundTripped).toEqual(original);
    // And the encoding really carries the exclusion, not just the inclusion.
    expect(encodeFilters(original)).toContain("not_project=vogt");
  });

  it("reads not_project as an exclusion only — never as a project inclusion", () => {
    // The whole point of a separate key: an exclusion can never be mistaken for
    // the inclusion it is the opposite of. A build without `not_project` in its
    // own URL_KEYS drops it (keys it does not own are left alone); this build
    // reads it, but only into the exclusion bucket.
    const filters = filtersFromQuery(queryFromSearch("not_project=vogt"));
    expect(filters.project).toBe("");
    expect(filters.exclude.projects).toEqual(["vogt"]);
  });

  it("drops a token whose key it does not understand", () => {
    // A future/stale negation syntax (an unknown key) is dropped per the
    // tolerance rule, and does not leak into any inclusion or exclusion.
    const filters = filtersFromQuery(
      queryFromSearch("not_bogusfacet=x&project=core&q=login"),
    );
    expect(filters.project).toBe("core");
    expect(filters.q).toBe("login");
    expect(excludeCount(filters.exclude)).toBe(0);
  });

  it("describes exclusions as negations for the lens tooltip", () => {
    const filters = filtersFromQuery(queryFromSearch("not_project=vogt&q=login"));
    const described = describeFilters(filters);
    expect(described).toContain("not project vogt");
    expect(described).toContain("search");
  });

  it("survives an empty query as the empty filter set", () => {
    const filters = filtersFromQuery(queryFromSearch(""));
    expect(excludeCount(filters.exclude)).toBe(0);
    expect(filters.q).toBe("");
    expect(encodeFilters(filters)).toBe("");
  });
});
