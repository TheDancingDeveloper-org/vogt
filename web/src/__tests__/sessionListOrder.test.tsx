// WI-2696 review: SessionList sorts the rows it is given, bookmark-free.
// A row that is not in the session store still renders, and a bookmark
// does not lift it the way the Places rail does.

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
  it("sorts the given rows oversight-first and keeps a row the store does not hold", () => {
    localStorage.setItem("vogt.sessionBookmarks.v1", JSON.stringify(["booked"]));
    const sessions = [
      row("booked", { name: "booked worker" }),
      row("over", { name: "overseer", role: "oversight" }),
      row("plain", { name: "plain worker" }),
    ];
    render(() => <SessionList sessions={sessions} />);
    const names = [...document.querySelectorAll(".session-row .name")].map((node) =>
      node.textContent?.replace("oversight", "").trim(),
    );
    expect(names).toEqual(["overseer", "booked worker", "plain worker"]);
  });
});
