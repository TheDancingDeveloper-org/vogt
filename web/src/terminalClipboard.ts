/**
 * Decode the payload of an OSC 52 clipboard-write sequence.
 *
 * OSC 52 is `ESC ] 52 ; <Pc> ; <base64> BEL` — a terminal program (Claude Code,
 * tmux, vim with `+clipboard`, …) asking the terminal to put `<base64>` on the
 * system clipboard. xterm.js parses it but takes no action unless a handler is
 * registered, so without one a program's copy is silently dropped — the program
 * reports it "sent N chars via OSC" and nothing reaches the clipboard.
 *
 * `data` is what xterm hands an OSC 52 handler: the sequence body *after* `52;`,
 * i.e. `<Pc>;<base64>`, where `Pc` selects the clipboard(s) — `c` (system), `p`
 * (primary), or a set like `cp`. Returns the decoded UTF-8 text for a write, or
 * `null` for anything we do not act on:
 *
 * - a **read request** (`<Pc>;?`) — deliberately not honoured: answering it lets
 *   a program *read* the user's clipboard over the wire, a data-exfiltration
 *   channel with no legitimate need here;
 * - an empty or malformed payload, or one that is not valid base64.
 */
export function decodeOsc52(data: string): string | null {
  const sep = data.indexOf(";");
  if (sep === -1) return null;
  const payload = data.slice(sep + 1);
  if (payload === "" || payload === "?") return null;
  try {
    const binary = atob(payload);
    const bytes = Uint8Array.from(binary, (ch) => ch.charCodeAt(0));
    return new TextDecoder().decode(bytes);
  } catch {
    return null;
  }
}
