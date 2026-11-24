import { Terminal } from "@xterm/headless";
import { SerializeAddon } from "@xterm/addon-serialize";
import { describe, expect, it } from "vitest";

import claudeCodeTui from "../../tests/fixtures/transcripts/claude-code-tui.bin.gz?gzbytes";
import shellPlain from "../../tests/fixtures/transcripts/shell-plain.bin.gz?gzbytes";
import cargoBuild from "../../tests/fixtures/transcripts/cargo-build.bin.gz?gzbytes";

// F5 (WI-129) persists a live pane's xterm state via @xterm/addon-serialize and
// restores it in a single `term.write` on reload. The fidelity contract is: a
// serialized screen, written into a fresh terminal, reproduces the same screen
// and scrollback — so restore-then-reserialize is a fixed point. Proven here
// against the real transcript corpus (H2), through the same core VT parser the
// browser build uses (@xterm/headless shares it).

const SERIALIZE_MAX_SCROLLBACK = 2000;

const CORPORA = [
  { name: "claude-code-tui", bytes: claudeCodeTui },
  { name: "shell-plain", bytes: shellPlain },
  { name: "cargo-build", bytes: cargoBuild },
] as const;

function makeTerminal() {
  const term = new Terminal({
    cols: 120,
    rows: 40,
    scrollback: 5000,
    allowProposedApi: true,
  });
  const serialize = new SerializeAddon();
  term.loadAddon(serialize);
  return { term, serialize };
}

function write(term: Terminal, data: Uint8Array | string): Promise<void> {
  return new Promise((r) => term.write(data, r));
}

describe.each(CORPORA)("serialized restore fidelity: $name", ({ bytes }) => {
  it("restore-then-reserialize is a fixed point (screen + scrollback preserved)", async () => {
    const a = makeTerminal();
    await write(a.term, bytes);
    const serialized = a.serialize.serialize({ scrollback: SERIALIZE_MAX_SCROLLBACK });
    // Note: an alt-screen program that has since exited leaves an empty normal
    // buffer, so `serialized` can legitimately be "" — the fixed-point below
    // still holds, and that is the property F5 relies on.

    // Restore into a fresh terminal in ONE write, then re-serialize.
    const b = makeTerminal();
    await write(b.term, serialized);
    const reserialized = b.serialize.serialize({ scrollback: SERIALIZE_MAX_SCROLLBACK });

    expect(reserialized).toBe(serialized);
    // The live screen (the viewport) matches too, independent of scrollback.
    expect(b.serialize.serialize({ scrollback: 0 })).toBe(
      a.serialize.serialize({ scrollback: 0 }),
    );
  });

  it("caps the serialized scrollback to the documented bound", async () => {
    const { term, serialize } = makeTerminal();
    await write(term, bytes);
    const capped = serialize.serialize({ scrollback: SERIALIZE_MAX_SCROLLBACK });
    // Never more scrollback lines than the cap (rows of viewport aside): the
    // serialized string's newline count stays bounded regardless of a
    // 5000-line buffer.
    const lines = capped.split("\n").length;
    expect(lines).toBeLessThanOrEqual(SERIALIZE_MAX_SCROLLBACK + term.rows + 2);
  });
});
