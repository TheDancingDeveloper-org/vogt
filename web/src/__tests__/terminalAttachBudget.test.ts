import { describe, expect, it } from "vitest";

import {
  MAX_INITIAL_PARKED_ATTACHES,
  initialAttachBudget,
  type AttachCandidate,
} from "../terminalAttachBudget";

function panes(n: number, visibleCount: number): AttachCandidate[] {
  return Array.from({ length: n }, (_, i) => ({
    id: `s${i}`,
    // Higher index = more recently active.
    lastActiveAt: i,
    visible: i < visibleCount,
  }));
}

describe("initial attach budget", () => {
  it("attaches only the visible panes plus the parked cap when 70 are live", () => {
    const { attach, defer } = initialAttachBudget(panes(70, 1));
    expect(attach).toHaveLength(1 + MAX_INITIAL_PARKED_ATTACHES);
    // The one visible pane, plus the three most recently active parked ones.
    expect(attach).toContain("s0");
    expect(attach).toEqual(expect.arrayContaining(["s69", "s68", "s67"]));
    expect(defer).toHaveLength(70 - attach.length);
    expect(defer).not.toContain("s0");
  });

  it("attaches every pane when they all fit under the cap", () => {
    const { attach, defer } = initialAttachBudget(panes(3, 1));
    expect(defer).toEqual([]);
    expect(attach).toHaveLength(3);
  });

  it("never defers a visible pane, however many are open", () => {
    const { attach, defer } = initialAttachBudget(panes(10, 6));
    for (let i = 0; i < 6; i += 1) expect(attach).toContain(`s${i}`);
    expect(defer.every((id) => !attach.includes(id))).toBe(true);
    expect(attach.length + defer.length).toBe(10);
  });
});
