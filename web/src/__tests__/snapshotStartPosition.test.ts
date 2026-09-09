import { describe, expect, it } from "vitest";

import { snapshotStartPosition } from "../terminalReplay";

describe("snapshotStartPosition", () => {
  it("is the payload start: end position minus payload length", () => {
    // A cold or warm snapshot ending at absolute position 1000 that carries
    // 400 bytes started at 600.
    expect(snapshotStartPosition(1000, 400)).toBe(600);
  });

  it("clamps at zero when the payload spans the whole stream so far", () => {
    // The whole ring is the payload (nothing has aged out): the start is 0, not
    // negative.
    expect(snapshotStartPosition(400, 400)).toBe(0);
    expect(snapshotStartPosition(400, 1000)).toBe(0);
  });

  it("re-anchors a reset that followed a stale resume cursor (F1)", () => {
    // The client had rendered up to 2_000_000 and reattached with that cursor.
    // The cursor aged out of a 4 MiB ring, so the server sent a bounded tail:
    // a 1 MiB payload ending at 5_000_000, reset:true. The client must discard
    // its stale cursor and re-anchor to the start of the tail it will replay.
    const scrollbackPos = 5_000_000;
    const scrollbackBytes = 1_048_576;
    const start = snapshotStartPosition(scrollbackPos, scrollbackBytes);
    expect(start).toBe(scrollbackPos - scrollbackBytes);
    expect(start).toBeGreaterThan(2_000_000); // well past the stale cursor

    // After replaying exactly the payload's bytes, the client's position is the
    // snapshot end again, so the live stream resumes with no gap or duplicate.
    expect(start + scrollbackBytes).toBe(scrollbackPos);
  });

  it("treats a zero-length snapshot as a no-op at the end position", () => {
    expect(snapshotStartPosition(1234, 0)).toBe(1234);
  });
});
