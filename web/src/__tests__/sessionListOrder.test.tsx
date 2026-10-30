// WI-2696 review: SessionList sorts the rows it is given. A row that is not
// in the session store still renders. Pinned (bookmarked) rows sit in their
// own strip above the rest, as on the overview list (WI-1736), and the rest
// keep oversight-first order.

import { describe, expect, it } from "vitest";
import { render } from "@solidjs/testing-library";
import type { SessionSummary } from "../api";
import { SessionList } from "../SessionList";

function row(id: string, over: Partial<SessionSummary> = {}): SessionSummary {
  return {
    id,
    name: id,
    activity: "idle",
    exit_code: null,
    scrollback_bytes: 0,
    cwd: "/workspace",
    created_at: "2026-08-18T08:00:00Z",
    role: "worker",
    ...over,
  };
}

describe("SessionList order (WI-2696)", () => {
  it("pins bookmarked rows first, then oversight-first, and keeps a row the store does not hold", () => {
    localStorage.setItem("vogt.sessionBookmarks.v1", JSON.stringify(["booked"]));
    const sessions = [
      row("booked", { name: "booked worker" }),
      row("over", { name: "overseer", role: "oversight" }),
      row("plain", { name: "plain worker" }),
    ];
    render(() => <SessionList sessions={sessions} />);
    // The aria-label leads with the session name (badges and tallies are not in it).
    const names = [...document.querySelectorAll("a.session-row")].map((node) =>
      node.getAttribute("aria-label")?.split(",")[0],
    );
    expect(names).toEqual(["booked worker", "overseer", "plain worker"]);
  });
});
