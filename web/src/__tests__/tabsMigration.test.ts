import { beforeEach, describe, expect, it } from "vitest";
import { migrateLegacyState } from "../tabs";

describe("old tab migration", () => {
  beforeEach(() => localStorage.clear());

  it("keeps machine panes, turns product tabs into recent routes, and preserves the active route", () => {
    const result = migrateLegacyState({
      active: "board",
      tabs: [
        { id: "term:s1", kind: "terminal", sessionId: "s1", label: "shell" },
        { id: "board", kind: "board", label: "Board" },
        { id: "workitem:WI-7", kind: "workitem", ref: "WI-7", label: "WI-7" },
        { id: "broken", kind: "unknown", label: "broken" },
      ],
    });
    expect(result.state.tabs.map((tab) => tab.id)).toEqual(["term:s1"]);
    expect(result.places.map((place) => place.path)).toEqual(["/board", "/w/WI-7"]);
    expect(result.initialRoute).toBe("/board");
  });

  it("is deterministic when run again, so a retry cannot duplicate routes", () => {
    const legacy = {
      active: "workitem:WI-7",
      tabs: [{ id: "workitem:WI-7", kind: "workitem", ref: "WI-7", label: "WI-7" }],
    };
    expect(migrateLegacyState(legacy)).toEqual(migrateLegacyState(legacy));
  });
});
