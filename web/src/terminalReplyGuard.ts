/**
 * WI-991: xterm.js answers terminal queries (CPR, OSC 10/11/4, DECRQM,
 * XTVERSION, kitty keyboard, XTGETTCAP, window ops) through `onData`, the
 * same channel as keystrokes. Replaying scrollback re-parses the queries a
 * program sent at start-up, and forwarding those answers would type them
 * into the running program. This module recognises such reports and tracks
 * whether a replay write is being parsed, so the pane can drop them.
 */

const ESC = "\x1b";
// One terminal report, anchored at the start of the remaining input.
const REPORT = new RegExp(
  "^(?:" +
    [
      // CPR / DECXCPR: ESC [ r ; c R, ESC [ ? r ; c (; p) R
      `${ESC}\\[\\??\\d+;\\d+(?:;\\d+)?R`,
      // DECRPM: ESC [ ? Pd ; Ps $ y (ANSI and DEC modes)
      `${ESC}\\[\\??\\d+;\\d+\\$y`,
      // DA1 / DA2 / DA3 replies: ESC [ ? … c, ESC [ > … c, ESC [ = … c
      `${ESC}\\[[?>=][\\d;]*c`,
      // Kitty keyboard flags reply: ESC [ ? flags u
      `${ESC}\\[\\?\\d*u`,
      // Window-op reports (CSI 14t / 18t …): ESC [ Ps ; Ps ; Ps t
      `${ESC}\\[\\d+;\\d+;\\d+t`,
      // OSC reports (colour queries): ESC ] … BEL | ESC \
      `${ESC}\\][^\\x07\\x1b]*(?:\\x07|${ESC}\\\\)`,
      // DCS reports (XTVERSION, XTGETTCAP, DECRQSS): ESC P … ESC \
      `${ESC}P[^\\x1b]*${ESC}\\\\`,
    ].join("|") +
    ")",
);

/** True when `data` is made only of terminal query replies, not keystrokes. */
export function isTerminalReport(data: string): boolean {
  if (data.length === 0) return false;
  let rest = data;
  while (rest.length > 0) {
    const match = REPORT.exec(rest);
    if (!match) return false;
    rest = rest.slice(match[0].length);
  }
  return true;
}

export type TermWrite = (data: string | Uint8Array, done: () => void) => void;

export interface ReplayReplyGuard {
  /** Wrap a replay write so replies generated while it parses are dropped. */
  write: (write: TermWrite, data: string | Uint8Array, done: () => void) => void;
  /** True when `data` is a query reply produced by replayed bytes. */
  shouldDrop: (data: string | ArrayBuffer) => boolean;
  /** True while a replay write is still being parsed. */
  active: () => boolean;
}

/**
 * xterm parses a write before calling its `done`, and emits query replies
 * synchronously while parsing, so a reply seen between a replay write and
 * its `done` was caused by replayed (or queued-behind-it) bytes.
 */
export function createReplayReplyGuard(): ReplayReplyGuard {
  let inFlight = 0;
  return {
    write(write, data, done) {
      inFlight += 1;
      let settled = false;
      write(data, () => {
        if (!settled) {
          settled = true;
          inFlight -= 1;
        }
        done();
      });
    },
    shouldDrop(data) {
      return inFlight > 0 && typeof data === "string" && isTerminalReport(data);
    },
    active: () => inFlight > 0,
  };
}
