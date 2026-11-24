import { describe, expect, it } from "vitest";

import { MAX_INITIAL_PARKED_ATTACHES } from "../terminalAttachBudget";

describe("initial attach budget", () => {
  it("warms only a handful of parked panes", () => {
    expect(MAX_INITIAL_PARKED_ATTACHES).toBeGreaterThan(0);
    expect(MAX_INITIAL_PARKED_ATTACHES).toBeLessThanOrEqual(3);
  });
});
