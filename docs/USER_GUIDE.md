# Vogt — User Guide

Status: **current as of 2026-08-15**, and describing one product. It replaced
`docs/engine/USER_GUIDE.md`, which was written in June 2026 for MyDevEnv2
standing alone and therefore documented terminals, the editor and git while
saying nothing about backlogs, boards, drift or audit — half a product, under
the wrong name.

**One honest caveat, stated once and not repeated.** Everything below is
implemented and covered by tests; the five Vogt surfaces are additionally
covered by 75 jsdom tests that drive them against a fake front door. What no
test here has done is *render* them in a browser or hold the phone — M11's and
M13's demos are outstanding and `ROADMAP.md` says so. Where a claim below is
about a layout, a gesture or a spoken word, it is a claim about code that has
been asserted and not yet watched working.

---

## 1. Getting in

1. Open **https://vogt.sprooty.com** — the merged product: Vogt's work
   surfaces and the session engine's terminals on one address.
   (Until the standalone stack is retired, https://mydevenv2.sprooty.com still
   answers and serves the session engine *without* Vogt — terminals and the
   assistant work, and every Vogt surface reports that no core is configured.)
2. Open **Settings (⚙)** and paste your bearer token.
3. **Save & reload.**

The token is a front-door token. What it can do is exactly what its
capabilities list says: reading needs only a valid token, and each kind of
write needs its own capability (`sessions`, `filesystem-write`, `git-write`,
`gui-control`, `agent-tasks-write`, `push-write`, `history-write`,
`assistant`, `vogt-write`). A token that authenticates but lacks the capability
gets a `403` — which means *this credential will never work for this route*,
as against a `401`'s *try a different one*.

Settings stores device-local **named auth profiles**, so you can keep a
read-only token and an interactive one side by side and switch between them
rather than leaving an admin token in a browser.

## 2. The surfaces

Everything is a tab, every tab is deep-linkable, and a pasted link restores the
view it names (FR-U11). Tabs persist per device.

| Surface | Link | What it is |
|---|---|---|
| **Board** | `#/board` | Work items in columns. The columns *are* the workflow's states, read from the server — not written down anywhere in the client. |
| **Backlog & bugs** | `#/backlog` | The ranked global views, with the explainable `why` behind each row. |
| **Projects** | `#/projects`, `#/projects?p=<slug>` | Per-project brief, CI, contract and compliance, the drift inbox, the dependency graph, and the import form. |
| **Work item** | `#/w/<ref>` | One item in full: description, state history, comments, relations, labels, evidence, and **Start session**. |
| **Audit** | `#/audit`, `#/audit?ref=WI-7` | Every write: who, what, and why. Filterable by actor, project, operation and time. |
| **Terminal** | `#/t/<session-id>` | A PTY, attached over WebSocket with scrollback replayed before live output. |
| **Editor** | `#/e/<path>` | Monaco over the workspace file API. |
| **Git** | `#/g/<repo>` | Status, diff, log, branch — and stage/unstage/discard/commit/checkout. |
| **Tasks** | `#/tasks` | Scheduled agent runs: create, edit, pause, resume, run now, inspect. |
| **History** | `#/history` | Archived scrollback from sessions that have ended, full-text searchable. |
| **Assistant** | `#/assistant` | The conversational surface — absent entirely unless the deployment provisioned it. |
| **GUI** | `#/gui` | The in-pod GUI stream, when one is configured. |

### 2.1 The board

Columns come from `workflow.list`, so a workflow change reshapes the board
without a client release. A drag is a `work.transition`: the card moves
immediately, and if the server refuses the move the card goes back and Vogt's
own sentence appears in the column the drop landed in. The client never keeps a
state the server refused.

- **Swimlanes** by project or initiative; per-column WIP counts; lanes and
  columns collapse, and the layout is remembered per device.
- **Filters** by project, state, type, label, initiative and actor, combinable,
  and written into the URL — so a filtered board is a link you can send. A
  combination can be named and recalled as a saved filter (per device; shared
  server-side filters are deliberately not built — `REQUIREMENTS.md` §3).
- **Quick-create** raises an item without leaving the board.
- The board is operable from the keyboard: focus an item, move it between
  columns, open its detail, quick-create.

**Every write asks for a reason, and will not submit without one.** That is not
a form-validation quirk; it is the product's rule (FR-W1). The reason is what
the audit log stores, and a board that armed one reason for a session is how
fifty audit rows end up saying "triage". A drop does pre-fill the last reason
the server accepted, which is a form with a required field rather than a
bypass; quick-create deliberately does not inherit it.

### 2.2 A work item

The detail page holds the description, the item's own state history (every
entry into a state, oldest first, with what it came from, when, who moved it
and why), comments, typed relations rendered as links, labels, and the
observed evidence collected against it — each with how fresh it is and how much
it is trusted.

A claim backed by a **still-running** session is marked provisional rather than
fresh. An observation whose payload does not carry the flag at all reads as
`unverified`, not as blank: a blank says "no opinion" when the honest answer is
"nobody checked".

**Start session** opens a PTY in the project's registered working tree, with
this item's brief — description, `why`, relations — written to a prompt file the
agent is pointed at. The session is linked back: the item shows its live
activity badge, and the terminal links back to the item.

### 2.3 Projects and the drift inbox

A project page shows its brief, CI status, contract compliance with the
criteria that failed, the dependency graph, the import form, and the drift
inbox.

The drift inbox shows **both sides of each disagreement, with provenance and
age, before any action is possible**. Accept and reject each collect a typed
reason. There is no bulk accept, on purpose: accepting drift is a declared-state
write, and making that convenient in bulk is exactly how the reason rule erodes.

A proposal may also be flagged **superseded**: a sweep newer than the proposal
no longer reproduces the condition that raised it. It is still open and still
yours to resolve — the flag says only that it is worth reading before the ones
without it, and it disappears again if the condition comes back. Nothing
closes itself.

The dependency panel distinguishes a project that references nothing from one
whose manifests are in a format Vogt does not read (`go.mod`, `pom.xml`) and
from one no sweep has walked. Where the same source lives in two registered
projects — a vendored crate that is also its own repository — both ends say so;
Vogt does not compare the copies or claim either has drifted.

### 2.4 The assistant

Absent unless the deployment set an API key — the routes answer `404` and every
client hides the surface, so an unprovisioned deployment looks unprovisioned
rather than broken.

It can read every session's scrollback and a curated read slice of Vogt
(`backlog`, `bugs`, `why`, `project.brief`, `project.list`, `work.get`,
`work.list`, `compliance`). It can propose four Vogt writes and typing into a
terminal — and **every one of those waits for you to approve it on screen**.
One pending action at a time, carrying the exact payload, the target, and the
reason that will be written to the audit log; it expires after 120 seconds and
a new message abandons it.

**Voice never approves.** Speech reaches the assistant only as a user message,
and an approval is a tap. Push-to-talk is *held*, not toggled — press to open
the microphone, release to send — because a take auto-sends, and a toggle left
on in a room with other people does not merely listen. The button is holdable
from the keyboard as well as by pointer.

An approved write is audited to **your** actor, using the core token paired with
the token that pressed approve. There is no shared "assistant" actor to fall
back to; an unpaired approver is refused by name.

### 2.5 Terminals

Sessions are owned by the server, so closing a browser does not end them and
several devices can watch one session at once. On attach you get a scrollback
snapshot and then live output; a reconnecting client can resume from its last
cursor and receive only what it missed.

Each session carries an activity state — `idle`, `running`,
`waiting-for-input`, `errored` — shown as a badge on the tab strip so an agent
waiting for approval is visible without opening it. **The state is a
heuristic** and should be read as a hint: `errored` is a real non-zero exit,
but `waiting-for-input` is a pattern match on the last ~512 visible bytes, and
a session that goes quiet without printing a recognizable prompt reads as
`idle`.

Session templates pre-configure command, cwd and environment, and can be
matched to a repository name or path prefix. Custom ones live in the engine's
TOML config:

```toml
[[session_templates]]
name = "Django Dev"
command = ["bash"]
cwd = "~/projects/myapp"
env = [["DJANGO_SETTINGS_MODULE", "myapp.settings.dev"], ["DEBUG", "1"]]
```

### 2.6 Scheduled agent tasks

A task is a command, a prompt, a schedule (`manual`, `interval`, or UTC
`daily`) and a persistent context file. Running one spawns a real PTY session,
so attach, scrollback, history, auth and push all work the way they do
everywhere else.

If an agent prints a line beginning with `MYDEVENV2_NOTIFY:`, two things
happen: a push notification goes out, and the line is **recorded on the run as
a finding**. The push is a delivery; the finding is the record, and it survives
a phone that was switched off.

A task may name a Vogt subject — a project slug, a work item ref, or both. A
bound run's findings become Vogt observations against that subject, with the
freshness and trust every other kind of evidence carries.

## 3. On a phone

The Android shell is a Capacitor wrapper whose WebView loads the deployed PWA,
so UI changes ship without a new APK; only native plumbing needs one.

- **Push** arrives for the things worth an interruption and nothing else by
  default: a session entering `waiting-for-input` or `errored`, new drift, and
  the agent-task notify hook. `idle_stall` and `agent_task_started` exist and
  default off. Quiet hours digest instead of sending.
- **Modifier row** above the soft keyboard: `Esc`, `Tab`, sticky `Ctrl`,
  arrows, `/ | ~`, `Enter`. Tap `Ctrl` then a letter to send `Ctrl+letter`.
- **Pinch** to zoom terminal font; the size is remembered per device.
- **Copy/paste**: long-press to select, long-press empty space to paste.
- The board renders as a list rather than columns below the narrow breakpoint.
- Tapping a notification lands on the terminal that raised it.

## 4. Keyboard reference

| Shortcut | Action |
|---|---|
| `Ctrl/Cmd+K` | Command palette |
| `Ctrl/Cmd+Shift+T` | New terminal session |
| `Ctrl/Cmd+Shift+W` | Close active tab |
| `Ctrl/Cmd+Alt+←/→` | Cycle tabs |
| `Ctrl/Cmd+Shift+C` / `V` / `A` | Copy / paste / select all in a terminal |
| Middle-click | Paste (Linux convention) |
| Right-click | Copy if there is a selection, else paste |
| `Ctrl/Cmd+S` | Save file (editor) |
| `Ctrl/Cmd+F` / `H` / `G` | Find / replace / go to line |
| `Alt+↑/↓` | Move line |
| `Ctrl/Cmd+D` | Add cursor at next match |

**The command palette reaches every read surface by fuzzy name** — projects and
work items included, so `rstnz` finds `rustnzb`. It never performs a write.
Mutating verbs whose collector is a *place* — quick-create, the drift inbox,
the import form — open that view and do nothing else; per-item verbs
(transition, comment, start session) are reached through the item's own entry,
which opens the page carrying all three forms.

## 5. When something is missing rather than broken

Vogt is two processes, and each keeps working when the other does not. This is
a designed property, not a degradation to be surprised by (FR-E9, FR-U21), and
the surfaces say which case they are in rather than rendering empty data as
truth.

| What is down | What still works | What you see |
|---|---|---|
| **vogt-core** | Terminals, files, git, history, the assistant over sessions | Vogt surfaces report the outage by name; they do not render an empty backlog |
| **the engine** | Every Vogt view, and the CLI/REST/MCP surfaces | Session controls disable with the reason stated |
| **the assistant** | Everything else | The tab is absent, not broken |
| **ContextKeeper** | Everything else | Terminals show as unprotected |

A lost event stream is indicated and reconciles on reconnect. A stale view
never presents itself as current — everything that aggregates says how old its
answer is.

## 6. Common problems

**Copy/paste does not work.** The clipboard API needs HTTPS and a permission
grant. Right-click always works as a fallback; on mobile, use the paste modal.

**A session will not start.** Check the token in Settings, and check that it
carries the `sessions` capability — a read-only token can list and read
sessions and cannot attach, because attaching writes to a PTY.

**A Vogt surface says the core is unavailable.** That is a real outage of a
real thing, not an unconfigured feature. `/readyz`'s `vogt_core` check is the
place to look, and it is deliberately non-fatal: restarting the container would
not revive the core and would kill every live PTY.

**The editor will not save.** Check that the token carries `filesystem-write`,
then check file permissions in the workspace.

**A write is refused with a reason you did not expect.** Read it — Vogt refuses
with the rule named. An illegal workflow transition, a missing reason, and a
scope the token does not hold all say which they are.

## 7. Frequently asked

**Can I use it offline?** No. Installed PWAs show an explicit offline fallback
page rather than pretending to support disconnected use — a queued offline
write could not carry an honest freshness answer, so none is offered
(`REQUIREMENTS.md` §3).

**Where are my files?** On the server, under the workspace root — the same tree
Vogt's project registry records paths in. That agreement is checked and
reported at `/readyz`.

**What happens if I close the browser?** Sessions keep running. Reconnect from
anywhere and the scrollback is replayed.

**Can several people share one instance?** It is single-tenant by design:
scopes are instance-wide, and there is no per-user server state. Each person
should have their own.

**How do I back up?** `vogt backup` takes the core's SQLite *and* the engine's
state directory — session history, push subscriptions, the VAPID keypair — as
one act, plus a manifest recording where the estate was (NFR-I6). Your own work
is in git, in the workspace.

**How do I update?** The PWA updates itself; hard-refresh if it seems stuck.
The deployment moves when a digest is bumped in the ops repository — publishing
an image and deploying it are separate acts on purpose.

## 8. Driving it from a terminal or an agent

Everything in this guide is also a CLI verb, a REST route and an MCP tool,
generated from one operation registry — that is what the parity tests assert.

```console
$ uv run vogt backlog
$ uv run vogt why --ref WI-1
$ uv run vogt contract check --project vogt --reason "before release"
```

For an agent, `vogt-mcp` speaks MCP over stdio against the same data directory,
and `vogt-mcp-remote` bridges stdio to a remote instance's `/mcp`. An agent's
tool list is exactly what its token may do — ungranted tools are absent rather
than present-and-refusing — and every write it makes requires a reason and
lands an audit row carrying it. `DEPLOYMENT.md` §7 is the full story of
reaching an instance from an agent environment.
