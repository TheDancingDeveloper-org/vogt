import { describe, expect, it } from "vitest";

import { shouldDeferCacheReplay } from "../terminalReplay";

describe("shouldDeferCacheReplay", () => {
  it("defers only a parked pane that has cached bytes", () => {
    // A retained-but-parked tab on reload keeps its cache in memory and replays
    // it lazily on activation, not into the shared FIFO with the active pane.
    expect(shouldDeferCacheReplay(true, true)).toBe(true);
  });

  it("does not defer the active pane", () => {
    // The pane the user is looking at replays its cache immediately.
    expect(shouldDeferCacheReplay(false, true)).toBe(false);
  });

  it("does not defer when there is nothing cached", () => {
    // No cache means nothing to replay: a parked pane with an empty cache just
    // waits to connect, it does not enter the deferred path.
    expect(shouldDeferCacheReplay(true, false)).toBe(false);
    expect(shouldDeferCacheReplay(false, false)).toBe(false);
  });
});
