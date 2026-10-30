// Gboard with autocorrect on typed words twice into the PTY: composition
// text reached onData mid-composition and again at commit (WI-2707).
import { describe, expect, it } from "vitest";

import { COMPOSITION_ECHO_WINDOW_MS, TerminalCompositionGuard } from "../terminalComposition";

/** Run a synthetic keyboard session; returns the bytes the PTY receives. */
function typeWords(words: string[]): string {
  const guard = new TerminalCompositionGuard();
  let pty = "";
  let now = 0;
  const onData = (d: string) => {
    const next = guard.filter(d, now);
    if (next) pty += next;
  };
  words.forEach((word, i) => {
    guard.start();
    for (let n = 1; n <= word.length; n++) {
      guard.update();
      // A misbehaving input path leaking provisional text mid-composition.
      onData(word.slice(0, n));
      now += 50;
    }
    const committed = guard.end(word, now);
    if (committed) pty += committed;
    // xterm's own compositionend echo of the same text, a tick later.
    now += 1;
    onData(word);
    now += 50;
    if (i < words.length - 1) onData(" ");
  });
  return pty;
}

describe("terminal IME composition guard", () => {
  it("delivers 'the quick brown fox' exactly once", () => {
    expect(typeWords(["the", "quick", "brown", "fox"])).toBe("the quick brown fox");
  });

  it("drops everything while composing", () => {
    const guard = new TerminalCompositionGuard();
    guard.start();
    expect(guard.filter("q", 0)).toBeNull();
    expect(guard.isComposing).toBe(true);
  });

  it("swallows a split echo and passes through what follows it", () => {
    const guard = new TerminalCompositionGuard();
    guard.start();
    expect(guard.end("brown", 0)).toBe("brown");
    expect(guard.filter("bro", 1)).toBeNull();
    expect(guard.filter("wn ", 2)).toBe(" ");
    expect(guard.filter("x", 3)).toBe("x");
  });

  it("passes the same text through once the echo window has passed", () => {
    const guard = new TerminalCompositionGuard();
    guard.start();
    guard.end("ls", 0);
    expect(guard.filter("ls", COMPOSITION_ECHO_WINDOW_MS + 1)).toBe("ls");
  });

  it("leaves non-composed input alone", () => {
    const guard = new TerminalCompositionGuard();
    expect(guard.filter("\r", 0)).toBe("\r");
    expect(guard.filter("\x1b[A", 1)).toBe("\x1b[A");
  });

  // Android drops compositionend on a keyboard/app switch or page hide; the
  // Terminal resets the guard on textarea blur, visibilitychange→hidden and
  // re-attach. Each path calls reset(); after it input flows again.
  for (const trigger of ["textarea blur", "visibilitychange hidden", "re-attach"]) {
    it(`a lost compositionend no longer swallows input after ${trigger}`, () => {
      const guard = new TerminalCompositionGuard();
      guard.start();
      expect(guard.filter("a", 0)).toBeNull();
      guard.reset();
      expect(guard.isComposing).toBe(false);
      expect(guard.filter("\r", 1)).toBe("\r");
      expect(guard.filter("\x03", 2)).toBe("\x03");
    });
  }

  it("a paste arriving mid-composition is not dropped", () => {
    const guard = new TerminalCompositionGuard();
    guard.start();
    guard.reset(); // paste event / programmatic paste path
    expect(guard.filter("\x1b[200~hi\x1b[201~", 0)).toBe("\x1b[200~hi\x1b[201~");
  });

  it("a fast identical keystroke after a commit is not swallowed as echo", () => {
    const guard = new TerminalCompositionGuard();
    guard.start();
    guard.end("a", 0);
    guard.keydown();
    expect(guard.filter("a", 10)).toBe("a");
  });

  it("reset clears a pending echo too", () => {
    const guard = new TerminalCompositionGuard();
    guard.start();
    guard.end("ls", 0);
    guard.reset();
    expect(guard.filter("ls", 1)).toBe("ls");
  });
});
