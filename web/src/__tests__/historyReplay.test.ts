import { describe, expect, it } from "vitest";

import { toReadableTranscript } from "../historyReplay";

describe("toReadableTranscript", () => {
  it("strips SGR colour sequences but keeps the text", () => {
    const raw = "\x1b[31mred\x1b[0m and \x1b[1;32mgreen\x1b[0m";
    expect(toReadableTranscript(raw)).toBe("red and green");
  });

  it("drops OSC title sets and keeps hyperlink labels", () => {
    const title = "\x1b]0;my session\x07";
    const link = "see \x1b]8;;https://example.com\x07the docs\x1b]8;;\x07 now";
    expect(toReadableTranscript(title + link)).toBe("see the docs now");
  });

  it("collapses an in-place spinner to its final frame", () => {
    const raw = "working |\rworking /\rworking -\rworking done\n";
    expect(toReadableTranscript(raw)).toBe("working done\n");
  });

  it("resolves a progress bar that redraws a shorter line", () => {
    const raw = "downloading 100%\rdone\n";
    // "done" overwrites the first four cells; the rest of the longer frame
    // remains, matching what the terminal would still be showing.
    expect(toReadableTranscript(raw)).toBe("doneloading 100%\n");
  });

  it("applies backspace edits", () => {
    expect(toReadableTranscript("abcX\bd")).toBe("abcd");
  });

  it("normalises CRLF and preserves real newlines", () => {
    expect(toReadableTranscript("line one\r\nline two\n")).toBe(
      "line one\nline two\n",
    );
  });

  it("removes cursor-movement and erase sequences", () => {
    const raw = "before\x1b[2K\x1b[1Gafter";
    expect(toReadableTranscript(raw)).toBe("beforeafter");
  });

  it("drops stray control bytes without eating tabs", () => {
    expect(toReadableTranscript("a\x00b\tc\x07d")).toBe("ab\tcd");
  });

  it("returns empty string for empty input", () => {
    expect(toReadableTranscript("")).toBe("");
  });
});
