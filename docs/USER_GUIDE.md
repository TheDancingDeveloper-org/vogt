# Vogt — User Guide

Vogt is a single-tenant work register for software projects: declared work
items, observed evidence about them, a ranked backlog that can explain itself,
and an audit log that records who changed what and why. It is reachable four
ways — a CLI, a web GUI, a REST API and an MCP server — all generated from one
operation registry, so anything this guide describes on one surface exists on
the others too (§8).

The web GUI, terminals, editor, git panel, scheduled agent tasks and the voice
assistant are served by the **engine**, the Rust front half of the stack
(`engine/`, `web/`; see [`ENGINE.md`](ENGINE.md)). Running the core on its own
— without the engine — still gives you everything in §2.1–2.4, §7 and §8 over
CLI, REST and MCP; the engine adds §1, §2.5–2.7, §3 and §4, and the browser
experience they live in. Install, configuration and the published Docker image
are covered in
[`GETTING_STARTED.md`](GETTING_STARTED.md) and [`DEPLOYMENT.md`](DEPLOYMENT.md);
every environment variable is listed in [`CONFIG.md`](CONFIG.md).

The engine was once a separate product, and a few deliberately invisible
compatibility identifiers keep the historical name: browser storage keys, some
Rust crate and config names, the Android package and notification-channel IDs,
and the agent-task notify phrase (§2.7). Legacy `MYDEVENV2_*` environment
names are still accepted as aliases for `ENGINE_*`. Renaming those would
discard local preferences, break push delivery, create a second Android app or
invalidate existing task definitions, so they stay; every user-facing label
says Vogt.

Status: **current as of 2026-08-30**, and describing one product.

**One honest caveat, stated once and not repeated.** Everything below is
implemented and covered by tests. The Solid surfaces have jsdom coverage
against a fake front door and automated Chromium coverage for representative
desktop and phone routes, including Sessions composition, terminal splitting,
zoom-event ownership and dirty-editor lifecycle. A human/device M13 demo is
still outstanding and `ROADMAP.md` says so.

---

## 1. Getting in

This section is about the PWA, which the engine serves. If you do not have the
stack running yet, [`GETTING_STARTED.md`](GETTING_STARTED.md) brings it up, and
[`ENGINE.md`](ENGINE.md) §3 covers the engine's own configuration.

**First run.** A brand-new instance — one whose core holds no tokens at all —
greets you with a setup wizard instead of the sign-in gate: name yourself and
it mints your first token, shows it exactly once alongside the CLI and MCP
equivalents, and offers to sign you straight in. That door closes itself the
moment any token exists (`GETTING_STARTED.md` covers the mechanism and the
headless equivalent). If your deployment's front door uses its own token
namespace (`ENGINE.md` §3), the wizard says so and hands you to the ordinary
sign-in below; the minted token remains your credential for the CLI, REST and
MCP surfaces.

Your first sign-in then lands on **Setup** (`#/setup`), which walks the two
remaining steps with a visible pass or fail on each: linking your own forge
PAT — upstream writes are then attributed to you, and the repositories your
credential can see become the import picker — and your first project, imported
from the forge or registered from a path already mounted into the instance,
followed by the first sweep and the collector coverage it earns. Every step is
skippable and nothing is wizard-only: Projects and Settings own the same
capabilities afterwards, and each write asks for a reason because it is
audited like any other.

1. Open your instance's address in a browser. The engine serves the GUI at
   `/` and proxies the core's API behind it, so work surfaces and terminals
   share one origin.
2. Open **Settings (⚙)** and paste your bearer token.
3. **Save & reload.**

The token is an **engine token** — the primary token the operator set in the
engine's config, or an entry in its `extra_tokens` list (`ENGINE.md` §3). What
it can do is exactly what its capabilities list says: reading needs only a
valid token, and each kind of write needs its own capability (`sessions`,
`filesystem-write`, `git-write`, `gui-control`, `agent-tasks-write`,
`push-write`, `history-write`, `assistant`, `vogt-write`). A token that
authenticates but lacks the capability gets a `403` — which means *this
credential will never work for this route*, as against a `401`'s *try a
different one*.

Vogt writes made through the GUI are performed with the **core token** the
operator paired with your engine token, so they are audited to your actor, not
to a shared one. Core tokens are issued with `vogt token issue` (see
`GETTING_STARTED.md`); the CLI, REST and MCP surfaces use them directly.

Settings is a routed modal. Whether it is opened from the desktop rail, the
phone bottom bar's **More** sheet, or the command palette, closing it returns
to the route that invoked it,
including filters in the query string; a direct `#/settings` link falls back
to Sessions. Browser Back closes a routed Settings view without adding a loop.

Settings stores device-local **named auth profiles**, so you can keep a
read-only token and an interactive one side by side and switch between them
rather than leaving an admin token in a browser.

Settings → **Theme** carries two pickers. **App Theme** recolours the whole
shell and takes effect immediately: choose **System** to follow your device's
light/dark setting, or pin one of **Vogt Dark** (the default), **Vogt Dim** (a
softer, warm-grey dark), **Vogt Light**, or **High contrast** in a dark or
light variant. The choice is remembered in this browser and applied before the
first paint, so the app never flashes the wrong colours on load. Below it, the
**Terminal Theme** picker sets the terminal palette; leave it and the terminal
follows the app theme, or pick one to pin it. Monaco (the file editor and diff
views) and the PWA's browser-chrome colour follow the app theme too.

The dialog scrolls its own body between a sticky section list and a sticky
**Cancel / Save** footer, so the footer stays reachable on a laptop and a
phone alike, and a header **×** closes it from the top. The Save button says
what it will do: an app- or terminal-theme change **applies immediately**; a
layout-mode
change is marked **(requires reload)** and the button reads *Save & reload*;
changing the token or base reads *Validate, save & reload*; a preferences-only
change reads *Save preferences*. You can save a preferences or layout change
**without touching the token field** — a blank-but-unchanged token is a valid
"keep the current credential", so a same-origin deployment that needs no token
can still change its settings. Destructive actions — signing out, clearing
managed browser data, deleting a saved profile, and the server retention
purges — each ask for confirmation first rather than acting on one click.

## 2. The surfaces

Board, Backlog, Inbox, Sessions, Projects and the work item share one
working-header order: the surface title, the freshness/evidence or connection
truth that qualifies its answer, view controls, then the primary action. That
truth is stated the same way everywhere — an age pill reading `<mode> — updated
Ns ago` (Live, Polling, Paused, Stale, or an outage), so how current a surface
is looks the same wherever you are. A surface can omit a control or action, but
it does not move the truth statement away from the answer it qualifies. On
narrow screens and at increased browser zoom those regions wrap in the same
reading and keyboard order. Longer implementation detail is available from the
header's disclosure where a surface needs it; unavailable, empty, loading, and
partial answers remain named in the surface's own words. Where a surface has
nothing to show, the empty state offers the next act rather than a dead panel:
an empty Board offers **Quick create**, **Clear filters** and a link to
register a project; an empty Backlog names the collector freshness it would
rank from and offers **Quick create**.

Board and Backlog both carry a **Refresh** cadence in their header — every
10/20/60 seconds, or off — beside a manual **Refresh now**; the age pill says
which is in force. Projects’ header keeps its Project / Dependencies / Drift
inbox / Import views as a segmented control that scrolls rather than wraps, so
Import stays reachable at any width, and a view that needs a project first says
so on the disabled tab.

Board, Backlog, Inbox, Projects, Audit, work items and Sessions are stable,
non-closable places. Sessions contains the terminal/editor panes and machine
tools; their existing deep links still select the named pane or tool, and a
pasted link restores the view it names (FR-U11). Open panes persist per device.

Inbox puts the server-ordered attention stream directly after its title,
freshness statement and compact source filter. Coverage/provenance and batch
operations remain immediately below the stream as labelled disclosures; an
empty answer opens its coverage automatically so “covered and empty” cannot be
mistaken for “not collected.” Entry and batch reason fields appear only after
you choose the corresponding write, and a refusal stays beside that composer
so the reason can be corrected without losing focus or context.

On a phone, the source filter becomes labelled pills and each entry keeps its
evidence in the stream while moving applicable writes into an **Inbox actions**
bottom sheet. The sheet is keyboard-operable, restores focus to its trigger on
cancel, and keeps archive, snooze, restore, adoption, suppression and drift
resolution reasons tied to the same server operations as the desktop view.

On desktop, the whole Places rail scrolls as one column when running sessions
make it taller than the window. Files therefore remains reachable rather than
being squeezed behind a separately scrolling session list; when visible it
keeps a 260px minimum containing its heading, search, rows and controls. The
rail's footer — **Settings**, **Sign out** and the connection indicator — is
pinned to the bottom of that scroller, so it stays reachable however long the
session list grows. You can drag the rail's right edge to resize it and
double-click that edge (or press **Home** with it focused) to return it to the
default width; **Ctrl/Cmd+B** hides and shows the rail. The compact Files
header keeps **New file** and **Refresh** on one row, while **New
folder** and **Upload files** sit under **More file actions**. Each row keeps
the name first, folders disclose nested rows with a caret, Git-changed files
show a quiet status letter such as `M`, and the labelled row menu retains
rename/move, duplicate, download, delete, upload-here, terminal and preset
operations where they apply.

Which folders are expanded, the search query and the editor sidebar's collapse
state are remembered, so switching to another tab and back leaves the tree as
you left it rather than collapsing to the root. A file operation refetches only
the affected folder, and the tree reconciles its rows and Git markers when the
workspace changes under it — a save, a Git action, a new file — and when the tab
regains focus. A session's row menu also offers **Open files here** and **Git
here**, which jump to that session's working directory in the file tree and the
Git tab.

| Surface | Link | What it is |
|---|---|---|
| **Board** | `#/board` | Work items in columns. The columns *are* the workflow's states, read from the server — not written down anywhere in the client. |
| **Backlog & bugs** | `#/backlog` | The ranked global views, with the explainable `why` behind each row. |
| **Projects** | `#/projects`, `#/projects?p=<slug>` | Per-project brief, CI, contract and compliance, the drift inbox, the dependency graph, and the import form. |
| **Work item** | `#/w/<ref>` | One item in full: description, state history, comments, relations, labels, evidence, and **Start session**. |
| **Audit** | `#/audit`, `#/audit?ref=WI-7` | Every write: who, what, and why. Filterable by actor, project, operation and time. |
| **Terminal** | `#/t/<session-id>` | A PTY, attached over WebSocket with scrollback replayed before live output. |
| **Editor** | `#/e/<path>` | Monaco over the workspace file API. |
| **Git** | `#/g`, `#/g/<repo>` | Choose a registered project, then inspect status, diff, log and branches or stage/unstage/discard/commit/checkout. |
| **Tasks** | `#/tasks` | Scheduled agent runs: create, edit, pause, resume, run now, inspect. |
| **History** | `#/history` | Every session, live and exited, in one list — each row badged live or exited and filterable by liveness; archived scrollback is full-text searchable. |
| **Assistant** | `#/assistant` | The conversational surface — absent entirely unless the operator configured an assistant API provider (§2.5). |
| **GUI** | `#/gui` | Desktop streaming (Selkies/KasmVNC) — hidden unless the engine has `GUI_STREAM_URL` set, was built with the feature, and the operator recorded an end-to-end verification with `GUI_STREAM_VERIFIED=true`. An old/direct link otherwise explains that the surface is unavailable. |

### 2.1 The board

Columns come from `workflow.list`, so a workflow change reshapes the board
without a client release. A drag is a `work.transition`: the card moves
immediately, and if the server refuses the move the card goes back and Vogt's
own sentence appears in the column the drop landed in. The client never keeps a
state the server refused. On a touch device, where there is nothing to drag,
each card carries a **Move…** control that opens the same reason composer with
a target-state select — the workflow's own legal edges for that card.

- **Swimlanes** by project or initiative; per-column WIP counts; lanes and
  columns collapse, and the layout is remembered per device. A lane is a
  grouping, not a move target: dropping a card into another lane of its own
  column changes nothing and says so, because changing an item's project or
  initiative is a different write — open the item to do it.
- **On a phone**, a labelled state row shows one workflow state at a time;
  the selected state is in the URL and every state keeps its exact count.
- **Filters** by project, state, type, label, initiative and actor are added
  through `+ Filter` and shown as individually removable chips. The URL remains
  the shareable/restorable source of truth, and a combination can be named and
  recalled as a saved lens (per device; shared server-side lenses are
  deliberately not built — per-user server state would make the instance
  multi-tenant, and it is not).
- **Quick-create** raises an item without leaving the board.
- Long cards expand and collapse in place so their full title and body remain
  readable without leaving the bounded, measured Board. An expanded body is
  rendered from Markdown, the same safe renderer the item page uses.
- The board is operable from the keyboard: focus an item, move it between
  columns, open its detail, quick-create. The full list of board keys lives in
  the `?` shortcut help rather than in a legend over the cards, so the first
  screen belongs to the work. A card is a button — a single click of its body,
  Enter, or Space opens it.

**Every write asks for a reason, and will not submit without one.** That is not
a form-validation quirk; it is the product's rule. The reason is what
the audit log stores, and a board that armed one reason for a session is how
fifty audit rows end up saying "triage". A drop does pre-fill the last reason
the server accepted, which is a form with a required field rather than a
bypass; quick-create deliberately does not inherit it.

### 2.2 The backlog and bugs

One tab, two ranked views over the same filter set. The order is Vogt's, not
the client's: `backlog` and `bugs` rank declared work and observed subjects
together, and every row can say why it is where it is.

- **Filters** by project, label, initiative, actor, type and state are added
  through `+ Filter` and shown as individually removable chips, exactly as on
  the board. The URL carries the set, and a combination can be named and
  recalled as a saved lens (per device). The **state** filter is the one
  exception: the ranked views take no state parameter, so it narrows the page
  already loaded rather than the estate. Its chip says so — `State: open · this
  page only` — and the count reads `N of M loaded rows`. Selecting rows and
  then changing the state filter keeps the selection: the rows are still on the
  loaded page, only hidden from view.
- **A row is as tall as what it says.** Rank, ref, trust, age and score stay on
  the row's first line; the title wraps rather than ending in an ellipsis.
  **Start a session…** sits on that collapsed line next to **More**, so it does
  not need the detail opened first. On a phone the row leads with its title and
  keeps a single compact meta line under it — `ref · kind · state · score`, with
  *why* an inline link — and the session act moves into the expanded detail. The
  row names its project rather than showing the slug, keeps the slug in the
  tooltip, and links it to that project's Projects page.
- **More** opens the rest of the row in place: an observed subject's
  provenance, the ranking factors behind its score, and the acts that row
  actually has. Declared rows offer Open and Select; observed subjects offer
  Adopt and Suppress. Nothing moves to a dialog.
- **A pull request collapses under the work it implements.** When a PR names a
  work item — through a `Closes #12` / `Fixes owner/repo#9` keyword in its body
  or title, or a branch named for the issue (`gh-12-…`, `wi-9/…`) — Vogt reads
  that as an `implemented_by` edge and lists the two as one stream: the item
  stays on the board and the PR folds under it rather than ranking beside it, so
  a single piece of work is counted once. The edge is observed, never typed in,
  and it only informs — unlike `depends_on` it does not block the item from
  completing. A PR whose target is not itself on the board still lists on its
  own, so nothing is silently dropped.
- **Bulk transition and bulk labelling** each collect their own typed reason,
  are applied one audited write at a time, and report each refusal against the
  item that was refused. The bulk bar is pinned to the top of the list while
  anything is selected, so it stays in reach after the reader has scrolled.

### 2.3 A work item

The detail page holds the description, the item's own state history (every
entry into a state, oldest first, with what it came from, when, who moved it
and why), comments, typed relations rendered as links, labels, and the
observed evidence collected against it — each with how fresh it is and how much
it is trusted.

The description and each comment are **rendered from Markdown** — headings,
lists, links, inline and fenced code — so an item that came from a forge reads
as it does there rather than as literal `#` and backticks. The renderer is
strict: it builds the formatting itself and never runs HTML or a
`javascript:`/`data:` link out of the text. A **Raw** toggle beside the
description shows the Markdown source when you need to copy or check it.

A claim backed by a **still-running** session is marked provisional rather than
fresh. An observation whose payload does not carry the flag at all reads as
`unverified`, not as blank: a blank says "no opinion" when the honest answer is
"nobody checked".

**The item is edited from this page, not only read.** A **Move to** control
under the state rail changes its state, offering the workflow's legal edges and
collecting the typed reason every write needs; the move renders optimistically
and rolls back visibly if the server refuses it. An **Edit** panel changes the
title, priority, assignee (a keyboard-operable picker over the actor roster),
effort, labels and description, through the same reason-collecting form. Adding
a comment is a third such form. The **Start a session** form is collapsed behind
a button so the page leads with the item rather than its machinery.

**Start session** opens a PTY in the project's registered working tree, with
this item's brief — description, `why`, relations — written to a prompt file the
agent is pointed at. The session is linked back: the item shows its live
activity badge, and the terminal links back to the item.

**Branches** the item is worked on appear in their own panel — but only when
there is one, so an empty panel never reads as "nothing is being worked on". A
branch belongs to an item when its name carries the item's reference:
`wi-7/…`, `feature/WI-7-…`, or, on a forge-linked project, `gh-264-…` for
issue #264. The convention is a configurable pattern
(`branch_binding_patterns`); the branch a session started *from Vogt* declares
it will use comes from `branch_binding_template` (default `wi-7`). You can also
declare a branch on an item yourself, without starting a session, with `work
bind-branch WI-7 --reason …`; omit the name to take the default from the
template, or pass `--branch feature/WI-7-…` to name your own. It only records
the name — Vogt never creates or renames a branch — and declaring one already
bound is a no-op. Each row
says where it came from — **declared** (a session said it would use it),
**observed** (a sweep found it in the checkout), or **both** — and an observed
branch says when it was last active ("active 2h ago") and how far it has
diverged from the default branch. A branch on one side only is marked **drift**:
Vogt *reports* where work is in git and never drives it, so declared and
observed are shown side by side rather than merged, and a disagreement between
them is surfaced rather than silently reconciled.

**The git story** reads those same branches and the pull request that
implements the item (its observed `implemented_by` edge) back as one answer to
*where is this in git?* Beside the workflow state sits a derived **phase** —
`no branch → branch active → PR open → in review → merged` — climbed as the git
evidence accumulates. It is a second opinion, never the state itself: a `merged`
phase on an item still `in_progress` is exactly the disagreement the phase is
meant to make visible. A **Git story** panel shows the pull request with its
derived state (`draft` / `open` / `in review` / `merged` / `closed`), its review
decision and checks rollup, and — like every collected fact on the page — how
fresh the observation is ("observed 4m ago from the forge"). The obvious
contradictions between the item and its git evidence read as **drift**: an issue
closed with a pull request still open, a pull request merged while the item is
still open, or a branch still active in a checkout for an item marked done.
Everything here is derived and read-only — Vogt reports where the work is in
git, it never writes the phase back onto the item. (The engine's per-task
run conclusion, #291, is not yet folded into the phase; it stays an engine-side
seam, so the phase is derived from the branches and the PR edge alone.)

The git story also feeds the ranking: an item with an open pull request or a
recently-committed branch is *moving*, and `why` shows it lifting above idle
work of the same priority — the branch's contribution decaying as its last
commit ages out of the activity window.

The page keeps itself current the way the board does: it **subscribes to the
change stream**, so a transition or a new comment somebody else made — and the
session's live-activity badge — arrive here without a manual refresh, and a tab
brought back from the background reconciles on return. The age badge reads
`Live` while it is listening. A refresh never lands on top of something you are
writing: while an edit, a comment or a session name is part-typed, the page
holds the current answer rather than swapping it out from under you.

### 2.4 Projects and the drift inbox

A project page shows its brief, CI status, contract compliance with the
criteria that failed, the dependency graph, the import form, and the drift
inbox.

The **registry list** filters by a name-or-slug search and sorts by name,
lifecycle or trust, all client-side — the estate returns every registered
project, so narrowing and ordering are presentation, not another query. A
project's **Work** panel is the way into that project's work: its counts are
links — open work to the Board scoped to the project, open bugs to the bugs
view, each by-state count to that state on the Board, and an Audit link to
every write against the project. Each is the same `?project=<slug>` (or
`?p=<slug>`) deep link a pasted URL restores.

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

### 2.5 The assistant

Absent unless the operator set an API key — the routes answer `404` and every
client hides the surface, so an unprovisioned deployment looks unprovisioned
rather than broken.

You type in a composer that grows with your message: **Enter** sends,
**Shift+Enter** starts a new line, and a pasted block keeps its line breaks. A
reply comes back rendered as Markdown — headings, lists, links and fenced code
— with a copy control beside it. While a reply is on its way a **Stop** button
replaces Send and cancels the request cleanly; a send that cannot reach the
engine is left in place marked unsent, with your text restored and a **Retry**.
**Clear** empties the whole conversation, and asks before it does.

The assistant talks to any **OpenAI-compatible chat endpoint**: the engine
reads `ENGINE_ASSISTANT_API_KEY`, `ENGINE_ASSISTANT_BASE_URL` and
`ENGINE_ASSISTANT_MODEL`, so a hosted provider, a proxy or a local server all
work if they speak that API. Voice is separate and also optional:
`ENGINE_ASSISTANT_STT_*` and `ENGINE_ASSISTANT_TTS_*` name speech-to-text and
text-to-speech endpoints, and without them the assistant is text-only. The
full table is in [`ENGINE.md`](ENGINE.md) §6.

**Voice without an account (optional).** You do not need a paid speech provider
to turn voice on. Layer the voice overlay and the engine gets a local,
CPU-only, OpenAI-compatible speech stack — a Whisper transcriber and a Piper
voice — running beside it in Compose:

```bash
docker compose \
  -f deploy/vogt.compose.yml \
  -f deploy/engine.overlay.yml \
  -f deploy/voice.overlay.yml \
  up --build -d
```

Nothing else to configure: the overlay points the engine's `STT`/`TTS` base
URLs at the two containers and picks models they serve. The first start
downloads a small Whisper model into a named volume, so give it a minute before
the microphone works. If you would rather use a hosted provider, skip the
overlay and set `ENGINE_ASSISTANT_STT_BASE_URLS` / `ENGINE_ASSISTANT_TTS_BASE_URLS`
(with a key and the provider's model/voice names) instead — the engine is bound
to neither. See [`CUSTOMISATION.md`](CUSTOMISATION.md#self-hosted-voice-sttts-as-an-optional-overlay).

It can read every session's scrollback and a curated read slice of Vogt
(`backlog`, `bugs`, `why`, `project.brief`, `project.list`, `work.get`,
`work.list`, `compliance`). It can propose four Vogt writes and typing into a
terminal — and **every one of those waits for you to approve it on screen**.
One pending action at a time, carrying the exact payload, the target, and the
reason that will be written to the audit log; it expires after 120 seconds —
the card counts the seconds down — and a new message abandons it.

The transcript separates newly received turns by time, keeps a tool trace on
one quiet line, and turns sessions returned by successful assistant tools into
state-labelled chips and **Open session** actions. Those links come from the
structured tool result, not from names guessed out of prose; older transcript
entries with no receipt timestamp simply render without a made-up time.
Terminal-input approval stays inline in the conversation and shows the exact
text, whether Enter will be appended, and the target session before **Deny**
and **Approve on screen** can be pressed.

**Voice never approves.** Speech reaches the assistant only as a user message,
and an approval is a tap. Push-to-talk is *held*, not toggled — press to open
the microphone, release to send — because a take auto-sends, and a toggle left
on in a room with other people does not merely listen. The button is holdable
from the keyboard as well as by pointer.

An approved write is audited to **your** actor, using the core token paired with
the token that pressed approve. There is no shared "assistant" actor to fall
back to; an unpaired approver is refused by name.

### 2.6 Terminals

Sessions are owned by the server, so closing a browser does not end them and
several devices can watch one session at once. On attach you get a scrollback
snapshot and then live output; a reconnecting client can resume from its last
cursor and receive only what it missed.

The Sessions place keeps an attention-sorted live roster beside an internal
tool bar. Terminal panes stay attached while you visit History, Tasks or
another place; inactive non-terminal tools unmount so their network requests,
listeners and large editor surfaces do not run in the background. Editor text
and view position are retained while switching tools, and any dirty editor
also activates the browser/PWA exit confirmation until it is saved.

**Saving is guarded against clobbering.** The editor remembers the version it
last read; a save that would overwrite a file changed on disk since then is
refused and shown inline as **File changed on disk** with **Overwrite** and
**Reload** — Overwrite forces your version past the guard, Reload takes the
disk's newer content. The toolbar's **Reload** button discards local changes
and re-reads at any time, and **Save** is disabled while the file is clean.
`Ctrl/Cmd+S` saves the active editor even when focus is on the tab bar, the file
tree or a split header rather than inside Monaco. If an unsaved draft is
restored on top of a file the disk has since moved past, the editor says
**Restored unsaved draft; disk differs** so the mismatch is not silent. The
toolbar also carries **Reveal in Git**, which opens the Git tab and selects the
file where a repository tracks it.

With no tool open, the Sessions overview is a live list of the running
sessions — name, activity dot and state word, and working directory — each a
link into its terminal, sorted with whatever wants attention first. When there
are no sessions at all it becomes a **Start a session** button beside the
configured presets, rather than an empty panel.

**New sessions are named for you and created straight away.** The **+ Session**
button, `Ctrl/Cmd+Shift+T` and a fresh-shell split take the working directory's
basename and deduplicate it — a second shell opened in `…/vogt` becomes
`vogt-2`, and a pane split off `vogt` reads as `vogt ▸2`. Hold **Shift** on
**+ Session** when you would rather type the name yourself.

**Splitting composes sessions, not only new ones.** *Split right* and *Split
down* open a small chooser: start a fresh shell in the current directory, or
pick any running session not already on screen — that session moves into the
new pane and nothing is spawned. Each pane in a split carries a header dropdown
that re-targets it at another session without disturbing the layout; choosing a
session already shown in another pane swaps the two. The command palette reaches
the same acts (*Split right with…*, *Show … in this pane*), and a session's rail
menu offers **Open beside current**. **Close pane** detaches a pane and leaves
its session running and listed — closing never kills; killing a shell is the
separate, confirmed **Kill pane**. **Broadcast** fans typing, paste and the
composer out to every pane at once, and **Maximise** lifts the active pane to
full size while the others keep running, hidden; both persist per tab. On a
phone one pane shows at a time, and the chooser is still reachable.

**Search the buffer** with `Ctrl/Cmd+Shift+F` or the toolbar's **Find** button:
a small find bar opens with the match count and next/previous controls (`Enter`
and `Shift+Enter` step through matches, `Esc` closes it). The toolbar also
carries an **A− / size / A+** font readout — the buttons stop at the 9–24 pt
limits and clicking the number resets to the default — and a color-theme
picker; both apply to every pane and persist across reloads.

**A dropped connection is visible and recoverable.** When the websocket drops
mid-session the terminal writes a single `[disconnected]` marker (once per
outage, not once per retry) and shows a *Reconnecting (try N, next in Xs)*
overlay with a **Retry now** button that reconnects immediately; anything you
typed while it was down is queued and its size is shown, then flushed on
reconnect. When a shell **exits**, an *Exited (code N)* banner offers **Restart
here** (a fresh shell in the same directory) and **Remove** — removing an
exited session skips the kill confirmation, since its scrollback is already
archived to History.

Each session carries an activity state — `idle`, `running`,
`waiting-for-input`, `errored` — shown in the Sessions roster so an agent
waiting for approval is visible without opening it. **The state is a
heuristic** and should be read as a hint: `errored` is a real non-zero exit,
but `waiting-for-input` is a pattern match on the last ~512 visible bytes, and
a session that goes quiet without printing a recognizable prompt reads as
`idle`.

Open `#/g` to choose a repository from Vogt's registered project list. The
chooser maps each project's declared absolute root into the engine's workspace
boundary; projects outside that boundary remain visible but unavailable. It
does not crawl the workspace or discover unregistered repositories. Choosing a
project writes its workspace-relative path into `#/g/<repo>`, so reload and
browser Back/Forward preserve the selection.

Each status row carries its own **+** / **−** to stage or unstage it, and a
**Stage all** beside the Status heading stages every change at once; staging a
file flips the diff to the index side rather than leaving you on the emptied
worktree view. **Commit staged** is disabled while nothing is staged, with a
hint saying so, and the diff toolbar's **Open file** opens the selected file in
the editor. Success banners clear themselves after a few seconds.

Git status, branches, commit history and the selected diff report failures in
their own panels and offer Retry. If a refresh fails after a successful read,
the previous answer remains visible but is explicitly marked stale. A clean
working tree, a repository with no branches or commits, a selected path that
is not a repository, and a missing/unavailable path are therefore separate
states rather than different ways to draw an empty panel.

History is the one place that lists every session, live and exited. Currently
running sessions from the live registry are unioned into the same list as the
archived ones (keyed by id, so a session that is both running and partially
archived appears once and reads as live), each row badged **Live** or
**Exited**, and the **Status** filter narrows the list to running, exited, or
archived-with-no-exit-code rows. A live row's replay pane tails its on-disk
terminal log directly, so you can watch a running session here without waiting
for it to exit; its output is not yet in the full-text search index, and its
archive-only Export and Delete actions appear once it has exited and been
archived. History loads archived sessions in pages and says how many are loaded
versus the server-reported total when that count is available. If no total is
available, a full page says that more may exist until **Load more** reaches a
short page. Metadata filters apply only to the pages already loaded; archived
output search runs across the server's full archive (up to the displayed result
limit) and is debounced, so typing a needle settles to a single server search
rather than one per keystroke. A failed archive, search, detail or replay read stays attached to that
panel with **Retry**. Previously loaded content remains visible but is marked
stale; an empty archive is shown only after a successful empty response.

Session templates pre-configure command, cwd and environment, and can be
matched to a repository name or path prefix. The presets you create in the
**Workspace Presets** editor are stored in this browser — they are not synced
across devices or shared with the engine, and the editor says so. Deleting a
preset asks first, and Escape will not discard a half-typed one. Presets the
operator ships as defaults live instead in the engine's TOML config:

```toml
[[session_templates]]
name = "Django Dev"
command = ["bash"]
cwd = "~/projects/myapp"
env = [["DJANGO_SETTINGS_MODULE", "myapp.settings.dev"], ["DEBUG", "1"]]
```

### 2.7 Scheduled agent tasks

A task is a command, a prompt, a schedule (`manual`, `interval`, or UTC
`daily`) and a persistent context file. Running one spawns a real PTY session,
so attach, scrollback, history, auth and push all work the way they do
everywhere else.

Task edits remain a draft until **Save Changes** or **Create Task** succeeds.
Selecting another task, starting a new draft, closing the Tasks tab, or leaving
the route asks whether to save, discard, or remain; a failed save keeps every
field. **Refresh** is a re-read, not a navigation, so it re-loads the list
without disturbing the draft rather than raising that question. The draft also
survives a full page reload — it is mirrored to the browser session, not held
only in memory. An unavailable first read is shown as an error rather than as
an empty task list. If a refresh fails after a successful read, the last task
list stays visible, is marked stale, and can be retried in place.

**Run Now** starts the task immediately. With an unsaved draft on screen it
reads **Save & Run**: it saves first, so the run uses the prompt you are
looking at rather than the last saved one. Either way the new run appears as a
row on the task — its session name is a link — instead of pulling you off Agent
Tasks into the terminal. A run whose session is still alive reads **Still
running** and settles to its real outcome on its own once the session exits.

If an agent prints a line beginning with the notify phrase (by default
`VOGT_NOTIFY:`; the older `MYDEVENV2_NOTIFY:` is still accepted for existing
task definitions), two things happen: a push notification goes out, and the
line is **recorded on the run as a finding**. The push is a delivery; the finding is the record, and it survives
a phone that was switched off. Findings are listed under their run row, so the
record is readable in the GUI and not only in a notification.

A task may name a Vogt subject — a **Vogt project** slug, a **Vogt work item**
ref, or both, entered on the task form. A bound run's findings become Vogt
observations against that subject, with the freshness and trust every other
kind of evidence carries.

**Fire a task from Vogt's own state.** Beyond the schedule, a task can carry
**triggers** that start a run when something happens in Vogt: a **work-item
transition** into a state you name (optionally filtered to a project, item kind,
label, or one item — the run is then bound to the item that moved), a **new
observation** of a kind you name, a **drift proposal** being raised, or a **PR's
checks** going green or red. Add each on the task form, enable or disable it
without losing its filter, and set **Max concurrent runs** to cap how many runs
of the task run at once. The engine subscribes to Vogt's event stream rather
than polling it. Two safety rules make triggers boring in the good way: a fire
that cannot start — the task is at its cap, or the item it would bind to is gone
— is **dropped, not retried**, so a burst never becomes a storm; and every
triggered run records **which trigger fired it and which event caused it**, shown
on the run row, so you (and `why`) can always see that "this run happened because
WI-7 entered ready". An `api` trigger arms a task to be fired programmatically
instead of by a person.

**Steer a run without killing it.** While a task's run is live, a **steer bar**
on its row sends a line of guidance to the agent, delivered the moment the run
next pauses at its prompt — you are not fighting it mid-thought. Tick **Interrupt
first** to cancel what the CLI is currently doing (Ctrl-C) before the guidance
lands. The same steering is available to the voice assistant as a tool, and a
steer is recorded with who sent it.

**Approval gates pause the run for a decision, and fail closed.** A task can
declare **gates**: named checkpoints — "Deploy to prod?", with options like
*Approve* and *Hold* — that a run stops at and holds the terminal until you
answer. The question shows on the row and on your phone; each option is a button
that sends its own reply into the run. The safety rule is that a gate never
approves itself by accident: a gate that is interrupted, times out, or whose
session dies resolves to **blocked**, never approved — an interruption is not a
yes. The one exception is a task set to **auto-approve**, which answers its own
gates with their affirmative option and records that it did so (as
`auto-approve`), so a run that approved its own checkpoints is always visible as
such. A resolved gate stays on the run as a one-line note — who approved which
option, or why it was blocked.

**Every run ends with a verdict and a conclusion.** When a run finishes its row
shows a typed **outcome** badge — *Succeeded*, *Failed*, *Partial*, *Skipped*,
or *Blocked* — next to the durable facts of what it did: how long it ran, the
exit code, the tip of the branch it worked on (a short sha) and the diff stats
for what it changed there (files, `+` insertions, `−` deletions), plus its cost
when the agent reported one. *Blocked* is a run that stopped at a gate that
failed closed; *Skipped* is a run that decided there was nothing to do (it
prints a `VOGT_SKIP:` line) rather than a run that did the work — the two are
worth telling apart.

**Ask for structured findings.** A task can carry an **output schema** — a JSON
Schema the run's findings must match. When set, the agent's findings block (a
fenced `json` block, or a file you name) is validated against it, and on a
mismatch the agent is **re-prompted to fix it**, up to a small budget, before
the run is recorded as *Partial*. Leave the schema unset and findings stay
free-text, exactly as before.

Daily schedules are read in **UTC**, and the daily-times field is labelled so —
a `09:00` is 09:00 UTC, not the reader's local morning.

## 3. On a phone

The PWA works in a phone browser as it is. The optional Android shell
(`mobile/`) is a Capacitor wrapper whose WebView loads your instance's PWA, so
UI changes ship without a new APK; only native plumbing needs one. It is built
from source, not published.

- **Push** arrives for the things worth an interruption and nothing else by
  default: a session entering `waiting-for-input` or `errored`, new drift, and
  the agent-task notify hook. `idle_stall` and `agent_task_started` exist and
  default off. Quiet hours digest instead of sending. Browser web-push needs
  only the engine's VAPID keypair, generated into its state directory on first
  run; push to the Android shell additionally needs
  `ENGINE_FCM_SERVICE_ACCOUNT_JSON`, and without it only browser subscriptions
  are delivered.
- **A session waiting for input becomes a card**, above the roster, showing
  the prompt it is actually waiting on. **Send y + Enter** and **Send Ctrl-C**
  send those keystrokes to that session's terminal — they are terminal input,
  not Vogt approvals, and the card says so. If the prompt cannot be read, or
  the session has exited, the card says that instead and offers nothing to
  press. Below the cards, the running sessions that are *not* waiting still
  list as rows, so an idle or busy session is one tap from its terminal.
- **A terminal and the Assistant own their phone headers.** On the overview,
  Sessions shows its kicker, title, live/stale connection truth and scrolling
  tool strip. Opening a terminal replaces that chrome with a compact session
  bar: Back, activity/name/path, Find and the overflow that retains terminal
  theme, font, broadcast, split and close actions. Assistant uses its own
  compact watch status and actions. Other machine tools retain the folded
  Sessions header, so no route stacks two headers over its work.
- **Open terminals form an identity-stable pager.** Swipe horizontally on the
  session bar, dots or terminal (a vertical gesture still scrolls), tap a
  state-coloured dot, or focus the bar and press Left/Right. The pager follows
  the full attention order, including exited sessions; if live activity
  reorders that list, the terminal you selected stays selected. Back always
  returns to the Sessions overview, and the bottom place bar stays out of the
  terminal screen.
- **The editor gives the code the width.** The Files sidebar is an overlay
  drawer that starts collapsed, so the editor gets the whole screen rather than
  a sliver beside a fixed column; **>** slides the drawer in over the editor and
  tapping the shaded area behind it (or **<**) closes it again. The split
  divider resizes on a touch drag, and a Git diff renders inline — one column,
  changes marked in place — instead of the side-by-side view that only fits a
  desktop's width.
- **The bottom bar reaches four places** — Sessions, Inbox, Board, Backlog —
  and a fifth **More** slot opens a sheet with the rest (Projects, Audit, Git,
  History, Tasks, and GUI stream and Assistant where those are enabled) plus
  **Settings** and **Sign out**. Every place and both account actions are two
  taps from any surface. The bar lifts above the soft keyboard rather than
  hiding under it.
- **Modifier row** above the soft keyboard: the first tier is `Esc`, `Tab`,
  sticky `Ctrl` and `Alt`, `^C`, Up, Down and `⋯`. The final key reveals the
  remaining arrows, `Home`/`End`/`PgUp`/`PgDn`, `/ | ~`, Enter, typing,
  selection, copy and paste controls. Tap `Ctrl` then a letter **on the soft
  keyboard** to send `Ctrl+letter` (tap `Ctrl` then `r` for `^R`); tap `Alt`
  then a letter for the `Esc`-prefixed sequence. An armed `Ctrl`/`Alt` clears
  itself after about five seconds, or the moment you use it.
- Use the terminal's labelled **A− / A+** controls for terminal-only font size;
  the size is remembered per device. Browser pinch/Ctrl+wheel zoom remains the
  normal whole-app zoom gesture.
- **Copy/paste**: long-press to select — a floating **Copy** chip appears over
  the selection; tap it to copy. To paste, use the modifier row's **Paste**
  button (iOS Safari does not raise the desktop long-press-to-paste menu).
- **Every control is a thumb's size.** On a phone, or on any touch screen, no
  button, tab, tick box or link in the navigation is smaller than 44 by 44
  pixels, and rows of them keep space between the targets rather than only
  inside them.
- **Nothing you type into is under 16px.** Below that, mobile browsers zoom the
  page when the field takes focus and the layout moves out from under you
  mid-sentence; the floor is set once, for every field in the product.
- The board renders as a list rather than columns below the narrow breakpoint.
- **Each surface arrives with its work on the screen.** Sessions leads with the
  session wanting attention, Inbox with the first attention row, Board with the
  selected state's cards, Backlog with the first ranked row. What a surface
  keeps for reference rather than for steering — a refresh cadence, the saved
  lenses — is one control away: **View controls** in the header, and `+ Filter`
  for the lens row.
- Tapping a notification lands on the terminal that raised it.

## 4. Keyboard reference

| Shortcut | Action |
|---|---|
| `Ctrl/Cmd+K` | Command palette |
| `Ctrl/Cmd+B` | Show or hide the Places rail (desktop) |
| `Ctrl/Cmd+Shift+T` | New terminal session |
| `Ctrl/Cmd+Shift+W` | Close active tab |
| `Ctrl/Cmd+Alt+←/→` | Cycle tabs |
| `Ctrl/Cmd+Shift+C` / `V` / `A` | Copy / paste / select all in a terminal |
| `Ctrl/Cmd+Shift+F` | Find in the terminal buffer |
| Middle-click | Paste (Linux convention) |
| Right-click | Copy if there is a selection, else paste |
| `Ctrl/Cmd+S` | Save file (editor) |
| `Ctrl/Cmd+F` / `H` / `G` | Find / replace / go to line |
| `Alt+↑/↓` | Move line |
| `Ctrl/Cmd+D` | Add cursor at next match |
| `j` / `k` | Inbox: focus the next / previous entry |
| `e` / `s` / `r` | Inbox: archive / snooze / resolve the focused entry |
| `?` | Open shortcut help outside text fields, editors and terminals |
| Board: `←`/`→`, `↑`/`↓` | Move focus across columns and within one (a card focused) |
| Board: `Shift`+`←`/`→` | Propose a move (the same reason prompt as a drop) |
| Board: `Enter` / `Space` / click | Open the focused card |
| Board: `n` | Quick-create a work item |

Desktop Places and the phone bottom bar share live workload counts. A numeric
zero means the corresponding canonical read completed and found none; an
ellipsis is still loading, a dashed retained value is refreshing, and an em
dash means that provider is unavailable. Sessions additionally calls out the
number waiting for input. Desktop session rows are keyboard links: focus a row
and press Enter or Space to open it, then Tab to the labelled bookmark,
duplicate and close controls.

**The command palette reaches every surface the Places rail and phone bar do,
and every read surface by fuzzy name** — projects and work items included, so
`myprj` finds `my-project`. Alongside the views, it navigates to **Open Inbox**,
**Open Sessions**, **Open History**, **Open Git**, **Open Tasks** and — when the
assistant is configured — **Open Assistant**, and it offers **Sign out**; the
keyboard is never a poorer map than the chrome. Matches are ranked, not merely
filtered: a name match always outranks one that only appears in a row's
description, so typing a session's name surfaces that session first, and
sessions rank ahead of work items. A command bound to a keyboard shortcut shows
it on its row, and the commands you have run most recently appear under
**Recent** at the top of the empty palette. **New File** opens a form with
separate workspace destination and filename fields; **Open File…** opens a
searchable workspace-file chooser whose results you can walk with the arrow keys
and open with Enter, without leaving the search box. Cancelling either returns
focus to the palette invoker, and creation writes only after **Create file**
succeeds — and refuses, rather than silently overwriting, if a file already
lives at that path.
Other mutating verbs whose collector is a *place* — quick-create, the drift inbox,
the import form — open that view and do nothing else; per-item verbs
(transition, comment, start session) are reached through the item's own entry,
which opens the page carrying all three forms: the **Move to** control changes
its state, the comment form adds a comment, and **Start a session** — collapsed
behind a button — opens one. Each collects the typed reason its write needs.

Opening the palette always focuses a blank query and selects its first result.
Arrow keys change the announced selection, Enter activates it, Tab stays inside
the modal, and Escape closes from anywhere in it and returns focus to the
button or shortcut that opened it. Pointer and touch activate the same command
rows. Shortcut help states where each binding applies; in particular `?` does
not take a literal question mark away from an input, editor or terminal.

Opening the palette renders its static and already-cached commands immediately.
Tasks, work items, and projects load independently, so one unavailable provider
does not block navigation commands. Prefix a query with `#` to discover
workspace project actions from manifests; that bounded scan is deferred until
it is requested and reused on later openings. Prefix with `/`, `>`, or `@` for
file, archived-output, or current-file symbol search. Typing `?` on its own
lists these prefix modes as a legend, so the vocabulary is discoverable once the
placeholder text has scrolled away. Closing or changing a special search cancels
its obsolete request. Choose **Refresh Command Palette
Data** when a write elsewhere makes a cached command list out of date.

Prefix a palette query with `>` to search archived terminal output. Choosing a
match opens a shareable History URL carrying the query, archived session and
selected excerpt; reload restores the same marked context, and Browser Back
returns to the route from which the palette was opened. The **Search History**
command opens History with its archived-output search field focused.

## 5. When something is missing rather than broken

Vogt is two processes, and each keeps working when the other does not. This is
a designed property, not a degradation to be surprised by, and the surfaces say which case they are in rather than rendering empty data as
truth.

| What is down | What still works | What you see |
|---|---|---|
| **vogt-core** | Terminals, files, git, history, the assistant over sessions | Vogt surfaces report the outage by name; they do not render an empty backlog |
| **the engine** | Every Vogt view, and the CLI/REST/MCP surfaces | Session controls disable with the reason stated |
| **the assistant** | Everything else | The tab is absent, not broken |

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

### The public demo

A deployment showing the persistent **Demo data** strip is the public demo,
not a connected Vogt instance. Its Board, Backlog, Inbox, Projects, Audit,
files, Git, tasks, Assistant and sessions are deterministic fictional data.
Writes last only in that browser tab. **Reset demo** restores the canonical
tour; Sign out shows the normal sign-in surface and a reload restores the
disposable demo identity.

The strip links directly to three terminal compositions: **Build + tests** is
a two-pane row, **Agent review** is a nested three-pane view, and **Incident
view** is a three-pane broadcast example. They use the real attach/replay
ordering, but the output is scripted. Input matches a small canned command
list and is never executed. The GUI stream is likewise a same-origin static
illustration, not a remote desktop. The source link in the strip names the
exact commit whose PWA asset hashes the demo advertises.

**Can I use it offline?** No. Installed PWAs show an explicit offline fallback
page rather than pretending to support disconnected use — a queued offline
write could not carry an honest freshness answer, so none is offered.

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
one act, plus a manifest recording what was backed up from where. Your own
work is in git, in the workspace. `DEPLOYMENT.md` covers restore.

**How do I update?** The PWA updates itself; hard-refresh if it seems stuck.
The server side moves when the operator pulls a new image (or rebuilds from
source) and restarts — publishing an image and deploying it are separate acts
on purpose, and `DEPLOYMENT.md` describes the upgrade order.

## 8. Driving it from a terminal or an agent

Everything in this guide is also a CLI verb, a REST route and an MCP tool,
generated from one operation registry — that is what the parity tests assert.

```console
$ uv run vogt backlog
$ uv run vogt why --ref WI-1
$ uv run vogt contract adopt --project vogt --reason "we keep these conventions"
$ uv run vogt contract check --project vogt --reason "before release"
$ uv run vogt project scaffold --project vogt --reason "close what the check found"
```

**The contract is something a project opts into.** A project you register is
not measured against it and is never called non-compliant: it reports
`not_applicable` until somebody runs `contract adopt`, and it is refused
nothing either way. Where a check does find a gap, it says what would close
it — `project scaffold` writes the missing skeleton without overwriting
anything already there, and where the content is a decision (which licence,
what belongs in `design/`) it says that instead of choosing for you. A
criterion your project cannot meet at all — a Cargo workspace has no root
`src/` — is declared with `contract inapplicable` and a reason, and stops
being counted as a failure while staying on the report.

The same operations are REST routes under `/api/` — send the core token as
`Authorization: Bearer …`; `GET /openapi.json` lists them. For an agent,
`vogt-mcp` speaks MCP over stdio against the same data directory
(`VOGT_DATA_DIR`), and `vogt-mcp-remote` bridges stdio to a remote instance's
`/mcp` using `VOGT_URL` and `VOGT_TOKEN_FILE`. An agent's tool list is exactly
what its token may do — ungranted tools are absent rather than
present-and-refusing — and every write it makes requires a reason and lands an
audit row carrying it.

The only optional integration on the core side is **GitHub** collection and
write-back, enabled by `VOGT_GITHUB_TOKEN_FILE`; absent means "GitHub was not
collected", never "no GitHub subjects". Vogt does not require or contact any
other external service. `CONFIG.md` has the variables.

## 9. Publishing an initiative to the forge

An initiative is a cross-project epic that lives in Vogt. That keeps it simple,
but it also makes it invisible to anyone who only has the forge open. `initiative
publish` closes that gap without giving up the simplicity: for each forge-linked
project the initiative spans, it creates — or, on a later run, re-adopts — **one
tracking issue** that carries a checkbox task list of the initiative's member
work items.

```console
$ uv run vogt initiative publish --slug platform-epic --reason "make the epic visible"
```

The tracking issue is labelled `initiative:<slug>` and its body looks like this,
one line per member — `- [ ] #<issue> <title>`, checked when the member is done
or won't-do:

```markdown
<!-- vogt:initiative:start -->
<!-- vogt:initiative:platform-epic -->

The platform epic.

### Work items

- [x] #41 Land the provider seam
- [ ] #52 Wire the collectors
<!-- vogt:initiative:end -->
```

Four rules make this safe to run again and again:

- **It only ever touches the managed region.** Anything you write in the issue
  *outside* the two `<!-- vogt:initiative:… -->` markers is yours and is preserved
  on every re-render — Vogt rewrites the block between the markers and copies the
  rest of the body through untouched. Adding or removing a member (with `work
  update --initiative`) re-renders the list in place.
- **It adopts, it does not duplicate.** The hidden marker in the body is how a
  second `publish` recognises the issue it already opened. You can even paste the
  two markers into an existing issue to hand it to Vogt.
- **It never closes the tracking issue.** Closing is somebody's call. When the
  initiative itself is closed, publishing *proposes* closing each tracking issue
  as a **drift proposal** you resolve by hand — Vogt does not write the close.
- **A tick upstream is drift, not a fight.** The tracking issue is observed like
  any other issue, so if a person ticks a box on the forge while the member is
  still open in Vogt (or the other way round), the next sweep surfaces the
  disagreement as `initiative_checkbox_drift` in the drift inbox — neither side is
  silently overwritten. The next re-render would restore the box; accepting or
  rejecting the proposal is how you decide which register is right.

A cross-project initiative gets one tracking issue per repo, each linking the
others. Publishing writes to the forge, so a project needs `forge writeback`
set to `full` and a linked repository first (`forge link`); a repo the
initiative touches that is not linked is reported as skipped, with the reason,
rather than dropped silently.
