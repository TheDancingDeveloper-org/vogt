# Roadmap and design notes

Forward-looking design decisions that are not yet work in flight. Each entry is
a recommendation with enough reasoning and numbers to act on — or to decide
"not now" without re-deriving the analysis.

## Engine-side headless VT per session (D6, WI-130)

*Spike, 2026-09-09. Part of the terminal-attach-budget initiative (WI-121).*

### Question

The engine keeps only **raw PTY bytes** in a per-session ring
(`scrollback.rs`), and every client rebuilds a terminal by re-parsing those
bytes into xterm.js on attach. Should the engine instead run a **headless
terminal emulator per session**, so a cold attach ships *screen state*
(tmux-style) — a serialized screen plus N lines of scrollback — instead of byte
history?

### What the budget work already fixed

Before committing to a VT, weigh it against what shipped in this initiative:

- **F1 (WI-125)** bounds *every* replay to the tail budget — the 4 MiB aged-out
  flood is gone; a cold or stale attach now ships at most the budget
  (default 1 MiB), ground-state aligned.
- **F3 (WI-127)** stops retained tabs from time-slicing the active pane's
  parser on reload.
- **F5 (WI-129)** persists a serialized xterm screen client-side, so a *reload*
  restores in one write with no raw re-parse, and (once F4 lands) a *switch*
  keeps the socket open and never re-streams.

So the two operator symptoms (switch re-stream, slow fresh open) are addressed
by bounding and by client-side serialization, **without** an engine VT. The VT
is only worth its cost if a residual problem remains after these ship and are
measured on prod — see "When to revisit".

### Crate options

| Crate | What it is | Fit for a screen-snapshot server VT |
|---|---|---|
| `vt100` | A minimal pure-Rust parser that maintains a screen grid + scrollback, no rendering. | **Best fit.** Smallest surface and memory; exposes the cell grid directly, which is all a snapshot needs. Serializing its screen to an xterm-compatible escape stream is a bounded amount of new code. |
| `alacritty_terminal` | The terminal model behind Alacritty. | Heavier per-cell model and an API shaped around a GPU renderer's needs; more memory and more moving parts than a snapshot server wants, and version churn tied to Alacritty. |
| `termwiz` | WezTerm's terminal library. | Full-featured (its own line/cell/attribute model, image protocols); the largest surface of the three. Overkill for "hold a screen and emit a snapshot". |

### Cost estimate (per session, at 8 busy sessions)

No live measurement was taken (an agent session cannot reach a prod stack; use
H1's load session — `load_session_command` in the engine integration tests — to
measure before adopting). Grounded estimate for `vt100`:

- **Memory.** A cell is a codepoint + attributes ≈ 8–16 B. A 200-col grid with
  50 visible rows + 1000 scrollback lines ≈ 210 000 cells ≈ **1.7–3.4 MB per
  session**, i.e. **~14–27 MB across 8 sessions** — *on top of* the existing
  4 MiB raw ring per session unless the ring is then shrunk. The ring cannot be
  dropped entirely: warm `resume_from` deltas and the history archive still read
  raw bytes.
- **CPU.** The VT must parse every byte the PTY produces, on the hot reader
  path. The client already parses at ~5–6 MB/s into xterm; a Rust grid VT is
  faster (tens of MB/s) but it is now paid **once per session, always**, not
  once per attach. For a chatty agent session this is continuous cost the
  raw-ring design does not have.

### Wire shape (if adopted)

Add a snapshot kind rather than changing the existing one:

```json
{"type":"snapshot-start","kind":"screen","session_id":"…","scrollback_pos":N,"reset":true}
```

followed by a server-produced, xterm-compatible escape stream that redraws the
screen and the last N scrollback lines, then live bytes as today. `resume_from`
still selects the byte-delta path (unchanged); `kind:"screen"` is the cold /
aged-out path only. The history archive is untouched (it keeps raw bytes).

### Fidelity

A server VT and the client xterm are two independent emulators; they can
disagree (edge cases in wide chars, unusual SGR, DEC private modes). The
snapshot is only correct if the server's redraw reproduces on xterm what the
program intended. The H2 corpus + fidelity harness is the right place to prove
this: parse each corpus in the candidate crate, emit the screen escape stream,
write it into headless xterm, and compare `serialize()` against xterm fed the
raw bytes. Adopt only if that comparison is clean across the corpus.

### Mobile

The prize on mobile would be dropping the IndexedDB cache entirely: if a cold
attach always ships a small screen snapshot, the phone need not persist raw
scrollback. But F5 already gives mobile a one-write restore from a *bounded*
serialized cache, so the marginal win is "no client cache at all" versus "a
small, capped client cache" — modest.

### Recommendation: **not now**

Ship and measure F1–F5 (and F4) on prod first. The engine VT doubles per-session
memory and adds continuous CPU on the PTY hot path to solve a problem the
bounding + client serialization work already targets. Revisit **only** if, with
those shipped and measured:

1. a first cold attach on mobile is still too slow *and* the ≤1 MiB tail is the
   bottleneck (not network), or
2. keeping any client-side scrollback cache proves untenable (storage, privacy),
   or
3. a new requirement wants server-authoritative screen state (e.g. server-side
   search over live screens, or thumbnails).

If revisited, the first step is a `vt100`-based measurement against H1's load
session at 8 sessions, then the H2 fidelity comparison above — no new work items
are filed now.
