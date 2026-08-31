// Turning an archived terminal log into something an operator can read.
//
// The History replay ships the raw session log — the exact byte stream a live
// terminal would have consumed — so it still carries colour, cursor moves,
// title sets, hyperlinks and the carriage returns that spinners and progress
// bars use to redraw a line in place. Dumped verbatim into a <pre> that is a
// wall of `\x1b[` noise (issue #490). This resolves the stream the way the
// emulator would have: escape sequences it would have acted on are removed,
// and carriage returns are applied so an in-place redraw collapses to the last
// state it settled on rather than one line per frame.

/** Strip the escape sequences a terminal consumes without printing: OSC
 *  (titles, OSC-8 hyperlinks), CSI (colour, cursor motion, erases) and the
 *  short two/three-byte escapes (charset selects, RIS, index). Printable text
 *  carried *between* sequences — the visible label inside a hyperlink, for
 *  example — is left in place. */
function stripEscapes(raw: string): string {
  return (
    raw
      // OSC: ESC ] ... terminated by BEL or ST (ESC \). Covers title sets and
      // OSC-8 hyperlinks; the link's visible text sits after the closer and
      // survives.
      // eslint-disable-next-line no-control-regex
      .replace(/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g, "")
      // CSI: ESC [ params intermediates final. Colour, cursor moves, line and
      // screen erases — none of which print a glyph.
      // eslint-disable-next-line no-control-regex
      .replace(/\x1b\[[0-9;?]*[ -/]*[@-~]/g, "")
      // DCS / SOS / PM / APC: ESC (P|X|^|_) ... ST. Device strings, rarely in
      // shell scrollback but cheap to drop cleanly.
      // eslint-disable-next-line no-control-regex
      .replace(/\x1b[P^_X][^\x1b]*\x1b\\/g, "")
      // Two/three-byte escapes: charset selects (ESC ( B), RIS (ESC c),
      // index/next-line (ESC M / ESC E), keypad modes, and the like.
      // eslint-disable-next-line no-control-regex
      .replace(/\x1b[()#][0-9A-Za-z]/g, "")
      // eslint-disable-next-line no-control-regex
      .replace(/\x1b[@-Z\\-_=>]/g, "")
      // Any escape left dangling (a truncated tail can cut one mid-sequence).
      // eslint-disable-next-line no-control-regex
      .replace(/\x1b/g, "")
  );
}

/** Apply carriage returns and backspaces within a single line so an in-place
 *  redraw shows its final state. A spinner that wrote "|", "\r/", "\r-" over
 *  one line collapses to the last frame instead of stacking three lines. */
function flattenLine(line: string): string {
  if (!line.includes("\r") && !line.includes("\b")) return line;
  const out: string[] = [];
  let cursor = 0;
  for (const ch of line) {
    if (ch === "\r") {
      cursor = 0;
    } else if (ch === "\b") {
      if (cursor > 0) cursor -= 1;
    } else {
      out[cursor] = ch;
      cursor += 1;
    }
  }
  return out.join("");
}

/** Control characters that carry no readable meaning once the emulator has run
 *  — kept out of the transcript so they do not render as replacement glyphs.
 *  Tab, newline and the carriage return / backspace handled above are spared. */
// eslint-disable-next-line no-control-regex
const STRAY_CONTROL = /[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/g;

/** Convert an archived raw terminal log into a human-readable transcript:
 *  escape sequences removed, in-place redraws resolved, newlines normalised. */
export function toReadableTranscript(raw: string): string {
  const stripped = stripEscapes(raw).replace(/\r\n/g, "\n");
  const lines = stripped.split("\n").map((line) => flattenLine(line));
  return lines.join("\n").replace(STRAY_CONTROL, "");
}
