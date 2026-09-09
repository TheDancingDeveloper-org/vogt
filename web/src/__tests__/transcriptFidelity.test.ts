import { describe, expect, it } from "vitest";

import claudeCodeTui from "../../tests/fixtures/transcripts/claude-code-tui.bin.gz?gzbytes";
import shellPlain from "../../tests/fixtures/transcripts/shell-plain.bin.gz?gzbytes";
import cargoBuild from "../../tests/fixtures/transcripts/cargo-build.bin.gz?gzbytes";

import { groundStateReplayStart } from "../terminalCache";
import { prepareReplayTail, sliceForReplay } from "../terminalReplay";

// Real PTY captures (see tests/fixtures/transcripts/README.md). These carry
// genuine escape sequences, SGR colour, cursor moves, alt-screen switches and
// UTF-8, so the ground-state trimming is proven against the byte patterns it
// actually meets — not synthetic `line\n` data.
//
// `lineOriented` corpora emit line feeds (the ground-state seam the trim aligns
// to). `claude-code-tui` is a pure alt-screen redraw stream that addresses the
// cursor directly and never emits `\n` — the case with NO seam, where the
// raw-byte trim can only fall back (and which F5's serialized-state restore,
// WI-129, is meant to cover). Keeping it in the corpus proves the trim stays
// source-exact even then.
const CORPORA = [
  { name: "claude-code-tui", bytes: claudeCodeTui, lineOriented: false },
  { name: "shell-plain", bytes: shellPlain, lineOriented: true },
  { name: "cargo-build", bytes: cargoBuild, lineOriented: true },
] as const;
const STEP = 4096;

const isContinuationByte = (b: number | undefined): boolean =>
  b !== undefined && (b & 0xc0) === 0x80;

describe.each(CORPORA)("transcript fidelity: $name", ({ bytes: corpus, lineOriented }) => {
  it("is a non-trivial real transcript with escape sequences", () => {
    expect(corpus.byteLength).toBeGreaterThan(STEP);
    expect(corpus.includes(0x1b)).toBe(true);
    expect(corpus.includes(0x0a)).toBe(lineOriented);
  });

  it("groundStateReplayStart picks a ground-state, source-exact seam at every 4 KiB cut", () => {
    let checked = 0;
    // Start at STEP: off === 0 is "nothing dropped" (outputPosition equals the
    // length), where a start of 0 is correct with no seam to align.
    for (let off = STEP; off < corpus.byteLength; off += STEP) {
      // Simulate the client ring dropping its oldest `off` bytes at an arbitrary
      // offset (which may split an escape or a UTF-8 char). outputPosition >
      // tail.length signals that a drop occurred.
      const tail = corpus.subarray(off);
      const start = groundStateReplayStart(tail, corpus.byteLength);

      if (start > 0) {
        // The only position we can prove is ground state: just past a line feed.
        expect(tail[start - 1]).toBe(0x0a);
        expect(isContinuationByte(tail[start])).toBe(false);
      } else {
        // start 0 on a dropped tail means there was no line feed to align to.
        expect(tail.indexOf(0x0a)).toBe(-1);
      }
      // The grounded tail is the exact same bytes of the source from off+start:
      // a view over the same buffer at the same offset, never a re-encoding.
      const grounded = tail.subarray(start);
      const expected = corpus.subarray(off + start);
      expect(grounded.byteOffset).toBe(expected.byteOffset);
      expect(grounded.byteLength).toBe(expected.byteLength);
      checked += 1;
    }
    expect(checked).toBeGreaterThan(4);
  });

  it("leaves a seamless (alt-screen) tail intact rather than cutting blind", () => {
    // Without a line feed there is no position the raw-byte trim can prove is
    // ground state, so groundStateReplayStart returns 0 (the tail is replayed
    // as-is) instead of guessing a cut inside an escape. Documented limitation
    // that F5 addresses by persisting serialized screen state.
    if (lineOriented) return;
    const tail = corpus.subarray(Math.floor(corpus.byteLength / 2));
    expect(tail.indexOf(0x0a)).toBe(-1);
    expect(groundStateReplayStart(tail, corpus.byteLength)).toBe(0);
  });

  it("prepareReplayTail bounds the tail, aligns it, and keeps it source-exact", () => {
    for (let off = STEP; off < corpus.byteLength; off += STEP) {
      const maxBytes = corpus.byteLength - off;
      const prepared = prepareReplayTail(corpus, corpus.byteLength, maxBytes);

      // A tail is a suffix of the corpus (same backing buffer, aligned at the end).
      const startAbs = corpus.byteLength - prepared.data.byteLength;
      expect(prepared.data.byteOffset).toBe(startAbs);
      expect(prepared.data.buffer).toBe(corpus.buffer);

      // Ground state: stream start, or just past a line feed; never a
      // continuation byte.
      if (startAbs > 0) {
        expect(corpus[startAbs - 1]).toBe(0x0a);
      }
      if (prepared.data.byteLength > 0) {
        expect(isContinuationByte(prepared.data[0])).toBe(false);
      }

      // Bounded to the budget whenever a newline seam exists at/after the cut;
      // a newline-free tail is documented to be left intact.
      if (corpus.indexOf(0x0a, off) !== -1) {
        expect(prepared.data.byteLength).toBeLessThanOrEqual(maxBytes);
      }
    }
  });

  it("sliceForReplay never begins inside an escape or a UTF-8 code point", () => {
    // A tighter budget forces a real cut on every corpus; the kept tail must
    // still begin in ground state.
    for (const budget of [16 * 1024, 64 * 1024, 128 * 1024]) {
      if (budget >= corpus.byteLength) continue;
      const tail = sliceForReplay(corpus, budget);
      const startAbs = corpus.byteLength - tail.byteLength;
      if (startAbs > 0) {
        expect(corpus[startAbs - 1]).toBe(0x0a);
        expect(isContinuationByte(tail[0])).toBe(false);
      }
    }
  });
});
