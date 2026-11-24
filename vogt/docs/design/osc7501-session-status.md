# Session status from OSC 7501

Status: design for WI-4319 on `rust-core` (the vogt-dev lane), 2026-11-16.

This note records what was found when Zellij 0.46's Attention Inbox and the
OSC 7501 status protocol were reviewed against how the engine works out a
session's state, and it sets the target state. It is a design, not an
implementation claim. Nothing here is built yet.

Code references are to `rust-core` at `022526e`. External facts come from the
Zellij 0.46.0 release post (2026-11-11) and its Inbox and Integrations
documentation pages. No standalone OSC 7501 specification was found, and the
agent versions below are as Zellij's documentation reports them (checked by
Zellij on 2026-11-09). They are not verified here; item 1 of the work
breakdown verifies them.

## Summary

The engine infers what an agent is doing from the outside: output timing, a
private window-title contract, and per-agent screen parsers. Agents now report
their own state in-band with OSC 7501. The target is for the engine to read
that report and treat it as the primary state signal for agents that emit it,
keeping the existing inference as the fallback for those that do not.

Zellij itself is not adopted. It is a local terminal multiplexer, the engine
is headless with web, mobile and pod clients, and the Inbox has no documented
API to integrate against. Its triage model is worth borrowing for Vogt's own
Sessions and Inbox surfaces.

## Decisions (operator, 2026-11-16)

1. **Scope: the signal goes in the engine.** The engine parses OSC 7501.
   Vogt's own surfaces borrow the Inbox triage ideas. Zellij runs nowhere.
2. **Agents in scope: Claude Code and the Codex fork.** opencode is out of
   scope for now and is a later item (see "Later").
3. **Authority: the reported state is primary straight away.** There is no
   shadow period. When an in-scope agent reports a state, it wins over the
   inferred one.
4. **Screen parsers stay, for answering.** OSC 7501 detects that a session is
   blocked. `approval.rs` still reads the menu so a dialog can be answered by
   choice.

## The protocol

A program reports its own state to the terminal:

```
ESC ] 7501 ; key=value:key=value ESC \
```

The sequence may end with BEL instead of `ESC \`.

| Key | Values |
|---|---|
| `state` | `idle`, `working`, `done`, `blocked`, `error`, or `clear` (removes the status) |
| `kind` | With `blocked`: `permission`, `question` or `auth` |
| `progress` | With `working` or `blocked`: 0 to 100 |
| `msg` | Base64 text, up to 2048 bytes decoded |
| `title` | Base64 label, up to 192 bytes decoded |
| `app` | Short program name, up to 32 characters |
| `id` | A task id for several tasks at once, up to 8 parts separated by `/` |

A program can ask what the terminal supports with `ESC ] 7501 ; ? ESC \`.
The reply is:

```
ESC ] 7501 ; ?:states=idle,working,done,blocked,error:kinds=permission,question,auth ESC \
```

Zellij also feeds its Inbox from signals the engine drops today: the terminal
bell and the desktop-notification sequences OSC 9, OSC 99 and OSC 777 (all
read as "needs you"), and the shell prompt markers OSC 133 / OSC 633 (command
finished, with exit code).

## How the engine works out state today

Three layers, each added to correct the one before:

- **Quiet window.** `activity::classify` (`engine/server/src/activity.rs:184`)
  reads output timing and tail patterns. `compute_activity`
  (`engine/server/src/pty.rs:2064`) calls it for every chunk.
- **Window title.** `TitleCapture` (`engine/server/src/screen.rs:155`)
  captures OSC 0 / OSC 2. `agent_title` (`screen.rs:488`) reads the Codex
  fork's private words (`codex: ready`, `codex: working`,
  `codex: awaiting approval`), and `titled_activity` (`pty.rs:2020`)
  reconciles them with the quiet window. Only the fork is read this way.
- **Screen parsing.** `approval::detect` (`engine/server/src/approval.rs:294`)
  runs per-agent parsers (`detect_codex`, `detect_claude_question`,
  `detect_numbered`, `detect_opencode`) behind a regex prefilter, and
  `screen::shows_prompt` (`screen.rs:354`) decides readiness.

Only OSC 0 / OSC 2 is captured. The bell, OSC 9 / 777 and OSC 133 are not
read.

Much of the tracker's session-state history is this inference being wrong in
one direction or the other: WI-1090 (running at an idle prompt), WI-1871 (a
question menu read as idle), WI-1598 (title and quiet window disagreeing),
WI-1355 (approval overlays the title does not name), WI-949 (opencode never
ready), WI-1314 (blocked reports cleared wrongly).

## Fit

| OSC 7501 | Engine today | Target |
|---|---|---|
| `working`, `idle`, `done` | Quiet window plus title | Reported state is primary; the quiet window is the fallback |
| `blocked` with `kind` | `approval::detect`, `Detected.kind` | Reported state is the detector; the parsers read the menu |
| `error` | Exit code only | New: an error from a live agent |
| Bell, OSC 9 / 777 | Dropped | Extra "needs you" source |
| `codex:` title contract | `agent_title`, `titled_activity` | Retired once the fork emits OSC 7501 |

## What OSC 7501 does not replace

- **Answering a dialog.** The report carries no options, no highlight and no
  countdown. `POST /api/sessions/{id}/answer` still needs the parsers. They
  run after a confirmed `blocked`, not as the detector.
- **Task-level done.** An agent's `done` means the turn ended, which is the
  engine's `ready`. `AUTOPILOT: DONE`, blocked reports
  (`POST /api/sessions/{id}/blocked`), escalations, budgets and hibernation
  stay the engine's own.
- **The change feed.** `changes.rs` and `wait-any` are unchanged in shape.
  They gain a more reliable source.

## Target state

### Engine

- The terminal parser's callbacks capture OSC 7501, the bell, and OSC 9 / 777
  alongside the title. vt100 0.16.2 (the version in `Cargo.lock`) has
  `unhandled_osc` and `audible_bell` on the `Callbacks` trait that
  `TitleCapture` already implements, so the parser does not change.
- Each session holds a **reported status**: state, kind, message, app, and
  when it was set. `state=clear` and process exit remove it.
- `compute_activity` uses the reported status first for an agent that emits
  it, and falls through to the quiet window and screen for one that does not:

  | Reported | `ActivityState` | Change feed |
  |---|---|---|
  | `working` | `running` | none |
  | `idle` | `idle`, or `waiting-for-input` at a prompt | `ready` |
  | `done` | as `idle` | `ready` |
  | `blocked` + `permission` / `question` / `auth` | `awaiting-approval` | `approval` |
  | `error` | `errored` | `errored` (decided in WI-4322, see below) |

- On `blocked`, the engine renders the screen and runs `approval::detect` to
  fill in the options. If the parser finds no menu, the dialog is reported
  with no options, as `title_approval_dialog` (`pty.rs:2051`) does today, and
  cannot be answered by choice.
- The bell and OSC 9 / 777 raise attention on a session that reports nothing
  else. They never lower it.
- The session summary and the `Activity` event carry the reported status, so
  clients can show the agent's own message.

### Rules that hold

- **An in-band signal never presses a choice.** Anything running in the PTY
  can emit OSC 7501, the same caveat the code already records for titles. A
  reported status can raise or clear attention. Answering still requires the
  parser to find a real menu on the screen.
- **A terminal state is never overridden.** `exited`, `errored`, `stopped`
  and `hibernated` win over any report, as they do over the title today.
- **The engine consumes queries itself.** If the capability query is
  answered, the engine answers it and strips it from what clients replay, so
  no client types a reply into the agent (WI-991).
- **A disagreement is logged.** The reported state is primary, but the engine
  logs when it and the inferred state differ, by session and agent, so a bad
  emitter can be diagnosed. This is diagnostics, not a shadow period.

### Agents

| Agent | Pin on `rust-core` | First version reported to emit | Work |
|---|---|---|---|
| Claude Code | 2.1.322 | 2.1.295 | None expected. Confirm it emits under the engine's PTY. |
| Codex fork | 0.160.0 | 0.163.0 (0.164.0 adds done, failed and waiting reasons) | Rebase the fork, then retire the `codex:` title contract |
| opencode | 1.18.31 | 2.1.0 (working, waiting and idle only) | Later |

### Surfaces

Triage features from the Inbox that Vogt's Sessions rail and Inbox do not
have: watch a session (every turn end counts as needing you), mute a session
or project, mark seen, and jump to the next session needing attention. Which
of these to build is an open decision.

## Open questions

1. **Emission gating.** Do Claude Code and Codex emit unconditionally, or
   only after the capability query is answered? Does either suppress it under
   `TERM=xterm-256color` (`pty.rs:1382`) or when it believes it is inside a
   multiplexer? Zellij's documentation lists
   `CLAUDE_CODE_DISABLE_TERMINAL_TITLE=1` as Claude Code's opt-out. The engine
   does not set it. Item 1 answers this, and the answer decides whether the
   engine must reply to the query.
2. **Pod sessions.** `engine/session-agent` streams PTY bytes to the engine,
   which parses them. Confirm that OSC 7501 arrives intact across that stream
   and across hibernate and wake.
3. **`error` from a live agent.** Decided (WI-4322): it maps to the existing
   `errored` activity state and `errored` change kind, not `blocked`. It is
   not sticky: the engine recomputes the state from the current report, so the
   next report (the agent's `idle` after a failed turn) or `clear` moves the
   session on. A report is authoritative for 30 minutes; after that, or once
   cleared or the process exits, the heuristics apply again.
4. **Nested emitters.** A tool the agent runs may emit its own OSC 7501 with
   an `id`. Agents usually capture tool output, so it should not reach the
   PTY. Item 1 should check.
5. **Which triage features to build**, and whether this work sits under
   WI-1317 (harness services) and WI-996 (one escalation model) or stands
   alone.

## Spike results (WI-4320)

Run 2026-11-16 under a python `pty` harness (120x40, no engine), trivial
prompts, `--permission-mode default`. Raw sequences are the fixtures in
`engine/server/tests/fixtures/osc7501/`. Claude Code 2.1.295 and 2.1.322 were
installed from npm into scratch (lockfile sha512 matches the registry); the
box's own 2.1.289 (a local install, not the engine pin, which is already
2.1.322 on `rust-2`) was also run. The Codex fork was 0.162.0 (installed), not
the 0.160.0 pin. Upstream `@openai/codex` 0.164.0 was also run at startup
(npm integrity verified); it is upstream, not the fork.

All captures came from a python `pty` harness, not from the engine. Nothing
here exercises the engine's PTY, `session-agent` or hibernate/wake.

### Observed sequences (Claude Code 2.1.322, query answered)

| Event | Bytes (all end in BEL) |
|---|---|
| startup | `ESC]7501;?` (the agent asks), then `state=idle:app=claude-code` |
| turn start | `state=working:app=claude-code` |
| tool running | `state=working:app=claude-code:msg=<b64 "Running ...">` |
| turn end | `state=done:app=claude-code` |
| permission dialog | `state=blocked:app=claude-code:kind=permission:msg=<b64 "approve Write: <path>">`; Esc (deny) then `state=idle` |
| question menu | `state=blocked:app=claude-code:kind=question:msg=<b64 question and options>`; Esc then `state=idle` |
| error (bad model) | `state=error:app=claude-code:msg=<b64 text>`, after `working` |

No `id`, `progress` or `title` keys were seen. No `auth` prompt was captured
(would need a logged-out run). The title sequence (OSC 0) keeps coming
alongside. No bell, OSC 9, OSC 99 or OSC 777 was emitted by Claude Code in any
run (default notification setting). 2.1.295 matches 2.1.322 for a turn. The local 2.1.289
has no OSC 7501 code and never sends the query (below the first emitting
version, 2.1.295).

### Answers

1. **Unconditional or after the query?** Gated on the reply. Claude Code
   (2.1.295 and 2.1.322) sends `ESC ] 7501 ; ? BEL` once at startup and emits
   no `state=` at all unless it is answered. The reply used was
   `ESC]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth ESC\`.
   Unanswered turns show only the query and OSC 0 titles. **The engine must
   answer the query**, or the agent stays silent. Note the query ends in BEL,
   not `ESC \`, and arrives before the first screen draw. The answer must be
   written to the PTY master input, and the query stripped from client replay
   (see WI-991). A reply is accepted with `ESC \` termination.
   Codex fork 0.162.0: sends no OSC 7501 query and emits none (no `]7501` in
   the binary). It still uses only the `codex: ready|working` title contract
   (plus OSC 10/11 colour queries at start). Upstream Codex 0.164.0 does
   contain emitter code (`ESC]7501;state=<s>:app=codex:kind=<k>`,
   `state=clear`, and a separate OSC 21337 `status=;indicator=;status-color=;detail=`
   sequence). At startup, with no auth and no model turn, it sent no query and
   no 7501, only OSC 10/11 colour queries. So whether 0.164.0 emits
   unconditionally or after a reply is **not answered**: it needs a
   authenticated turn. That is deferred to WI-4331 with the rebased fork
   (0.169, WI-4323). Claude Code's gating is the only answered case.
2. **Suppressed by TERM or multiplexer?** No. Identical sequences under
   `TERM=xterm-256color`, `TERM=dumb`, and `TERM=tmux-256color` with `TMUX`
   set. `CLAUDE_CODE_DISABLE_TERMINAL_TITLE=1` is the opt-out: with it the
   agent sends neither the query nor any state. The engine does not set it
   (grep of engine env found no use; verify when implementing).
3. **Pod sessions through `session-agent`, hibernate and wake?** Not
   verified, and split to WI-4331: the harness is not the engine and has no
   pod. WI-4331 also captures the rebased Codex fork.
4. **Nested emitters?** Not leaked. A Bash tool printed
   `ESC]7501;state=blocked:...:id=tool1` and `ESC]9;...` plus a BEL via
   `python3 -c`; none reached the PTY (Claude Code captures tool output). The
   only 7501 lines in that run were the agent's own, with no `id`. Codex not
   tested.

### Codex limits

The Codex backend returned "high demand / Reconnecting" for every 0.162.0
turn, so no completed turn, approval dialog or question was captured for the
fork. Only startup, working/ready titles and the interrupted-turn error path
were recorded. 0.164.0 was captured at startup only (no credentials here).
Fork 0.169 (WI-4323) is captured in WI-4331.

### Consequences for the design

- Item 2 must include the query reply, written once per session start,
  before any other output handling, for Claude Code.
- The engine's Claude Code pin on `rust-2` is already 2.1.322, which emits, so
  no pin bump is needed. The "None expected" work for Claude Code stands, with
  the reply added.
- `blocked` carries a `msg`, so the message text is available without the
  screen parser.
- Codex gating, auth prompt and the pod/hibernate path are open (WI-4331).

## Work breakdown

In order. Items 2 and 4 can run in parallel once item 1 has reported.

1. **Spike: capture what Claude Code and Codex emit.** Run each under the
   engine's PTY at the pinned and target versions, record the raw sequences
   for a turn, a permission dialog, a question and an error, and answer open
   questions 1, 2 and 4. Output: captured transcripts added to the parser
   corpus (WI-123) and a short findings section in this note.
2. **Engine: capture OSC 7501, bell and OSC 9 / 777.** Parser callbacks, the
   per-session reported status, the contract fields, and the query reply if
   item 1 says one is needed. No change to activity yet.
3. **Engine: reported status is the primary activity signal.** The mapping
   above in `compute_activity`, the disagreement log, parsers demoted to
   reading the menu after `blocked`, and the rules that hold. Depends on 2.
4. **Codex fork: rebase to 0.164.0 or later** and bump the pin in
   `engine/agent-versions.env`.
5. **Retire the `codex:` title contract.** Remove `agent_title`,
   `titled_activity` and `title_approval_dialog` once the fork emits OSC 7501
   on vogt-dev. Depends on 3 and 4.
6. **Surfaces: triage features.** Watch, mute, mark seen, next attention, and
   the agent's own message on the session row. Needs the decision in open
   question 5.

## Later

- **opencode.** Needs 1.18.31 to 2.1.0, a major version, and reports only
  working, waiting and idle. `shows_opencode_prompt` and `detect_opencode`
  stay until then, and stay for done and failed after it.
- **OSC 133 / 633 prompt markers** for plain shell sessions, to report a
  finished command and its exit code.
- **Forwarding status outward**, so an operator's own terminal that
  understands OSC 7501 shows a session's state when attached.

## Sources

- Zellij 0.46.0 release post: https://zellij.dev/news/terminal-inbox-session-previews/
- Inbox and Attention: https://zellij.dev/documentation/inbox.html
- Integrations (agent support table): https://zellij.dev/documentation/inbox-integrations.html
- vt100 0.16.2 `Callbacks`: https://docs.rs/vt100/0.16.2/vt100/trait.Callbacks.html
