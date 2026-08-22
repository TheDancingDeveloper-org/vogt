import { describe, expect, it } from "vitest";
import { surfaceHref, type RecentPlace } from "../tabs";

// #215: opening a work item unmounts Board/Backlog. Returning via the
// rail/palette/bottom-bar (a bare `#/board`) remounts them against an empty
// query and loses the filter set. The links instead carry the last query that
// surface wrote, recorded by `rememberPlace` as `path+search`.
describe("surfaceHref — rail/palette/bottom-bar return target", () => {
  const board = (search = ""): RecentPlace => ({
    path: `/board${search}`,
    label: "Board",
  });

  it("returns the remembered filtered URL for a surface (empty-query return applies it)", () => {
    const places = [board("?kind=feature"), { path: "/w/WI-7", label: "WI-7" }];
    expect(surfaceHref(places, "/board")).toBe("/board?kind=feature");
  });

  it("falls back to the bare path when the surface has never been visited", () => {
    expect(surfaceHref([], "/board")).toBe("/board");
    expect(surfaceHref([{ path: "/w/WI-7", label: "WI-7" }], "/board")).toBe(
      "/board",
    );
  });

  it("lets the most recent visit win — a later bare /board clears an older filter", () => {
    // rememberPlace prepends most-recent, so a deliberate bare navigation sits
    // ahead of the stale filtered entry and is the one the link should carry.
    const places = [board(""), board("?kind=feature")];
    expect(surfaceHref(places, "/board")).toBe("/board");
  });

  it("matches by pathname, so a later filtered visit wins the same way", () => {
    const places = [board("?kind=bug"), board("?kind=feature")];
    expect(surfaceHref(places, "/board")).toBe("/board?kind=bug");
  });

  it("keeps Board and Backlog independent", () => {
    const places = [
      { path: "/backlog?why=WI-1", label: "Backlog" },
      board("?kind=feature"),
    ];
    expect(surfaceHref(places, "/board")).toBe("/board?kind=feature");
    expect(surfaceHref(places, "/backlog")).toBe("/backlog?why=WI-1");
  });
});
