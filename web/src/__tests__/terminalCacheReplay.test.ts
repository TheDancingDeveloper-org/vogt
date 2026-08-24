import { describe, expect, it } from "vitest";

import { groundStateReplayStart } from "../terminalCache";

// Issue #366: the client scrollback cache is a byte-oriented ring, so its
// oldest bytes are dropped at an arbitrary offset. Replaying such a tail must
// begin at a terminal ground state, never inside an escape sequence or a UTF-8
// multibyte character.
describe("groundStateReplayStart", () => {
  it("does not trim a cache that still holds the whole stream", () => {
    // outputPosition == length => nothing was ever dropped; byte 0 is ground.
    const data = new Uint8Array([0x61, 0x0a, 0x62]); // "a\nb"
    expect(groundStateReplayStart(data, data.byteLength)).toBe(0);
  });

  it("advances past the first newline for a dropped-front tail", () => {
    // A chopped "\x1b[7m" tail ("7mHI") followed by a real line.
    const data = new Uint8Array([0x37, 0x6d, 0x48, 0x49, 0x0a, 0x58, 0x59]); // "7mHI\nXY"
    // outputPosition > length signals the ring dropped older bytes.
    expect(groundStateReplayStart(data, 1000)).toBe(5); // start at "XY"
  });

  it("never begins on a UTF-8 continuation byte", () => {
    // "é" is 0xC3 0xA9; a cut landing on the continuation byte then a newline.
    const data = new Uint8Array([0xa9, 0x0a, 0x62]); // orphan cont. byte, "\nb"
    const start = groundStateReplayStart(data, 1000);
    expect(start).toBe(2);
    expect((data[start]! & 0xc0) !== 0x80).toBe(true);
  });

  it("leaves a newline-free tail untouched (no whole-buffer drop)", () => {
    const data = new Uint8Array([0x61, 0x62, 0x63]); // "abc", no newline
    expect(groundStateReplayStart(data, 1000)).toBe(0);
  });
});
