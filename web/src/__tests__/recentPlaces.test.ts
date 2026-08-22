// Recent-places dedupe and labelling (#245). Two rules, both pure and both
// asserted here rather than through the shell: a surface is one place however
// its filters vary, and a terminal place is named by its live session and not
// by the opaque id in its URL.

import { describe, expect, it } from "vitest";
import { addPlace, placePath, recentPlaceLabel } from "../tabs";
import type { RecentPlace } from "../tabs";

describe("addPlace dedupes by surface path", () => {
  it("keeps one chip per surface, with the latest search", () => {
    const places: RecentPlace[] = [];
    addPlace(places, "/board?project=a", "Board");
    addPlace(places, "/board?project=b", "Board");

    // Two filtered visits to the Board are one place, not two identical chips.
    expect(places).toHaveLength(1);
    expect(places[0]).toEqual({ path: "/board?project=b", label: "Board" });
  });

  it("keeps distinct surfaces apart and moves the newest to the end", () => {
    const places: RecentPlace[] = [];
    addPlace(places, "/board", "Board");
    addPlace(places, "/backlog?view=bugs", "Backlog");
    addPlace(places, "/board?project=x", "Board");

    expect(places.map((place) => place.path)).toEqual([
      "/backlog?view=bugs",
      "/board?project=x",
    ]);
  });

  it("treats a query-less path as its own surface", () => {
    expect(placePath("/inbox")).toBe("/inbox");
    expect(placePath("/inbox?source=drift")).toBe("/inbox");
  });
});

describe("recentPlaceLabel names a terminal by its session", () => {
  const labels = { "/board": "Board" };
  const roster: Record<string, string> = { s2: "my build" };
  const sessionName = (id: string) => roster[id];

  it("uses the live session name for a /t/:id chip", () => {
    expect(recentPlaceLabel("/t/s2", labels, sessionName)).toBe("my build");
  });

  it("falls back to the id when the roster does not hold the session yet", () => {
    expect(recentPlaceLabel("/t/s9", labels, sessionName)).toBe("s9");
  });

  it("ignores a query when resolving the terminal id", () => {
    expect(recentPlaceLabel("/t/s2?focus=1", labels, sessionName)).toBe("my build");
  });

  it("keeps the known route label for a non-terminal place", () => {
    expect(recentPlaceLabel("/board?project=a", labels, sessionName)).toBe("Board");
  });

  it("reads a work item ref out of its own path", () => {
    expect(recentPlaceLabel("/w/WI-7", labels, sessionName)).toBe("WI-7");
  });
});
