# Terminal transcript corpus

Real PTY byte streams used by the parser-fidelity tests
(`src/__tests__/transcriptFidelity.test.ts`) and, later, the browser replay
budget spec (H3, WI-124) and the serialized-restore fidelity compare (F5,
WI-129). They exist so the ground-state trimming that both the server ring and
the client cache rely on is proven against **real** escape sequences, SGR
colour, cursor moves, alt-screen switches and UTF-8 — not synthetic `line\n`
data.

Each file is gzip-compressed (`.bin.gz`); the tests `gunzip` them in memory.

| Fixture | Source | Exercises |
|---|---|---|
| `claude-code-tui.bin.gz` | a curses TUI redraw loop under a PTY | alt-screen enter/leave, cursor addressing, SGR colour, OSC title, box-drawing + emoji UTF-8 — the shape of an agent TUI |
| `shell-plain.bin.gz` | `ls -la --color=always -R` + UTF-8 text under a PTY | coloured `ls` output, prompts, `\r`, multibyte box/CJK/emoji characters |
| `cargo-build.bin.gz` | a real `cargo build -v` (clean local crates) | green-bold `Compiling`/`Fresh` status lines, `\r`, long verbose rustc command lines with paths |

## How these were captured

The **intended production source** is a real dev-stack session:

```
GET /api/history/:id/download        # raw recorded PTY bytes for a session
```

An agent session cannot reach a running stack (its token has no engine
`sessions` capability — see the terminal-attach plan), so the checked-in
fixtures were captured from real programs on a developer box with
`scripts/capture_transcript.py` (a `pty.fork` capture), for example:

```sh
python3 scripts/capture_transcript.py tui.raw   -- python3 tui.py           # a curses redraw loop
python3 scripts/capture_transcript.py shell.raw -- bash -lc 'ls -la --color=always -R /usr/include; …'
python3 scripts/capture_transcript.py cargo.raw -- bash -lc 'cargo clean -p … ; cargo build -v --color always'
```

Every capture is then run through the sanitiser, which rewrites home paths,
tokens, JWTs and email addresses to inert placeholders **and asserts that no
secret pattern survives** before it is committed:

```sh
python3 scripts/sanitise_transcript.py tui.raw tui.bin
gzip -9 tui.bin        # -> claude-code-tui.bin.gz
```

To refresh from a real stack, download a transcript, run it through
`sanitise_transcript.py --check` (it must pass), sanitise, gzip, and replace the
file here — keeping the same three shapes.

## Sizes

These local stand-ins are smaller than a full-session download (the TUI and
`cargo` captures in particular): the fidelity tests sample every 4 KiB, so a few
hundred KB per corpus already gives dozens of independent cut offsets across
real escape/UTF-8 boundaries. Replace them with larger real-session downloads
when a stack is available.
