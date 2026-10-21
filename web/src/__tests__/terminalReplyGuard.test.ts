import { describe, expect, it } from "vitest";
import { Terminal } from "@xterm/headless";
import { createReplayReplyGuard, isTerminalReport } from "../terminalReplyGuard";

// The start-up queries opencode 1.18.31 sends (WI-991).
const QUERIES =
  "\x1b[6n\x1b[6n\x1b]10;?\x07\x1b]11;?\x07\x1b]4;0;?\x07\x1b[>0q" +
  "\x1b[?2004$p\x1b[?2026$p\x1b[?u\x1bP+q544e\x1b\\\x1b[14t\x1b[c";

describe("isTerminalReport", () => {
  it.each([
    "\x1b[12;40R",
    "\x1b]11;rgb:0000/0000/0000\x1b\\",
    "\x1b]10;rgb:ffff/ffff/ffff\x07",
    "\x1b[?2004;2$y",
    "\x1b[?1;2c",
    "\x1b[>0;276;0c",
    "\x1bP>|xterm.js(6.0.0)\x1b\\",
    "\x1bP0+r\x1b\\",
    "\x1b[?0u",
    "\x1b[4;600;800t",
    "\x1b[1;1R\x1b]11;rgb:0000/0000/0000\x1b\\",
  ])("recognises %j", (data) => {
    expect(isTerminalReport(data)).toBe(true);
  });

  it.each(["a", "\r", "\x1b[A", "\x1b[200~hi\x1b[201~", "\x03", "\x1b[1;1Rx", ""])(
    "passes keystrokes %j",
    (data) => {
      expect(isTerminalReport(data)).toBe(false);
    },
  );
});

describe("replay reply guard", () => {
  function harness() {
    const term = new Terminal({ cols: 80, rows: 24, allowProposedApi: true });
    const guard = createReplayReplyGuard();
    const sent: string[] = [];
    term.onData((data) => {
      if (!guard.shouldDrop(data)) sent.push(data);
    });
    const write = (d: string | Uint8Array, cb: () => void) => term.write(d, cb);
    return { term, guard, sent, write };
  }

  it("drops replies to queries in replayed scrollback", async () => {
    const { term, guard, sent, write } = harness();
    await new Promise<void>((resolve) => {
      guard.write(write, new TextEncoder().encode(`hello${QUERIES}world\r\n`), resolve);
    });
    expect(sent).toEqual([]);
    expect(guard.active()).toBe(false);
    term.dispose();
  });

  it("still answers live queries after the replay", async () => {
    const { term, guard, sent, write } = harness();
    await new Promise<void>((resolve) => guard.write(write, "\x1b[6n", resolve));
    await new Promise<void>((resolve) => term.write("\x1b[6n", resolve));
    expect(sent).toEqual(["\x1b[1;1R"]);
    term.dispose();
  });

  it("keeps keystrokes typed while a replay is in flight", () => {
    const { term, guard, write } = harness();
    guard.write(write, "x".repeat(10_000), () => {});
    expect(guard.active()).toBe(true);
    expect(guard.shouldDrop("a")).toBe(false);
    expect(guard.shouldDrop("\x1b[1;1R")).toBe(true);
    term.dispose();
  });
});
