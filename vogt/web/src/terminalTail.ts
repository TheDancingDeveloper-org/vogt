// Reading a terminal's own tail, and guessing nothing else about it.
//
// Two surfaces answer a session that is waiting for input — a work item's
// sessions panel and the phone's Sessions place — and both have to show the
// prompt before they offer a way to answer it. The decoding is the same in
// both, so it is here rather than in whichever file needed it first.

/** How much of the tail to show. Enough for a prompt and the lines that set
 *  it up, not so much that the answer scrolls off a phone. */
const TAIL_LINES = 12;

/** How many trailing scrollback bytes to fetch for a tail. Far more
 *  than TAIL_LINES needs even with escape sequences, but a tiny fraction of a
 *  full ring — so the card no longer pulls and base64-decodes up to 4 MiB. */
export const TAIL_FETCH_BYTES = 16 * 1024;

/** A row with nothing to read: whitespace, box drawing, block and dot glyphs. */
const DECORATION = /^[\s\u2500-\u259f\u00b7\u2022\u2026.\u22ef]*$/;

/** The tail of a session's scrollback, decoded and stripped of the escape
 *  sequences a terminal would have consumed. */
export function tailOf(scrollbackBase64: string, maxLines = TAIL_LINES): string {
  let raw: string;
  try {
    const bytes = Uint8Array.from(atob(scrollbackBase64), (c) => c.charCodeAt(0));
    raw = new TextDecoder().decode(bytes);
  } catch {
    return "";
  }
  const plain = raw
    // CSI and OSC sequences: colour, cursor moves, title sets. A prompt
    // rendered with them intact is unreadable in a <pre>.
    // eslint-disable-next-line no-control-regex
    .replace(/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g, "")
    // A full-screen TUI (Claude Code's permission prompt among them) spaces
    // words with cursor-forward and starts rows with cursor moves rather than
    // writing spaces and newlines. Dropped outright, its prompt runs together
    // or vanishes (WI-1830), so keep what those two moves meant.
    // eslint-disable-next-line no-control-regex
    .replace(/\x1b\[(\d*)C/g, (_, n: string) => " ".repeat(Math.min(Number(n) || 1, 200)))
    // eslint-disable-next-line no-control-regex
    .replace(/\x1b\[[0-9;]*[HfEB]/g, "\n")
    // eslint-disable-next-line no-control-regex
    .replace(/\x1b\[[0-9;?]*[ -/]*[@-~]/g, "")
    .replace(/\r\n/g, "\n")
    .replace(/\r/g, "\n");
  // Rows that carry no words — blanks, box rules, dot leaders — are what a
  // TUI frame is mostly made of; a tail of them reads as an empty box.
  const lines = plain
    .split("\n")
    .map((line) => line.trimEnd())
    .filter((line) => !DECORATION.test(line));
  return lines.slice(-maxLines).join("\n");
}

/** Does the tail end in something that reads like a yes/no question?
 *
 *  Only used to *offer* the two one-tap answers beside the free-text box —
 *  never to answer anything, and never to hide the prompt. A wrong guess
 *  here costs a button that is not useful, which is the correct direction
 *  for a guess about somebody else's prompt to fail in. */
export function looksLikeYesNo(tail: string): boolean {
  const last = tail.trimEnd().slice(-200).toLowerCase();
  return /\(y\/n\)|\[y\/n\]|\(yes\/no\)|\by\/n\b|\?\s*\(y[/|]n\)/.test(last);
}
