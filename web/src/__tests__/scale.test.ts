import { describe, expect, it } from "vitest";
import { createDemoState } from "../demo/fixtures";
import { scaleDemoState, STRESS_PROFILE } from "../demo/scale";

describe("#422 browser stress profile", () => {
  it("creates the checked-in board, tree and rail sizes deterministically", () => {
    const scaled = scaleDemoState(createDemoState(), STRESS_PROFILE.workItems);
    expect(scaled.work).toHaveLength(STRESS_PROFILE.workItems);
    expect(Object.keys(scaled.sessions)).toHaveLength(STRESS_PROFILE.sessions);
    expect(Object.keys(scaled.files)).toHaveLength(STRESS_PROFILE.files + 7);
    expect(scaled.files["stress/depth-01/depth-02/depth-03/depth-04/depth-05/depth-06/depth-07/depth-08/file-0008.ts"])
      .toBeDefined();
    expect(scaleDemoState(createDemoState(), STRESS_PROFILE.workItems).work[1999]?.ref)
      .toBe("WI-2000");
  });
});
