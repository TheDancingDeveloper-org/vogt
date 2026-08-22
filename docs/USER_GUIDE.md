# Vogt — User Guide

Vogt is the product name everywhere a person encounters it: the browser and
route titles, sign-in screen, installed web/Android app and notifications. The
installed PWA keeps `/` as its manifest identity, so an installation formerly
labelled MyDevEnv2 upgrades in place to Vogt instead of appearing as a second
app. Refresh an already-open window once after deployment to receive the new
service worker, offline page and notification artwork.

Some deliberately invisible compatibility identifiers retain the historical
name: browser storage and event keys, Rust crate/config names, the Android
package and notification-channel IDs, and the `MYDEVENV2_NOTIFY:` task hook.
Which of those are renameable and which are migrations is inventoried on
[#144](https://github.com/TheDancingDeveloper-org/vogt/issues/144).
Changing those as presentation copy would instead discard local preferences,
break protocols or FCM, create a second Android app, or invalidate existing
task definitions. Their user-facing labels and messages still say Vogt.

Status: **current as of 2026-08-18**, and describing one product. It replaced
`docs/engine/USER_GUIDE.md`, which was written in June 2026 for MyDevEnv2
standing alone and therefore documented terminals, the editor and git while
saying nothing about backlogs, boards, drift or audit — half a product, under
the wrong name.

**One honest caveat, stated once and not repeated.** Everything below is
implemented and covered by tests. The Solid surfaces have jsdom coverage
against a fake front door and automated Chromium coverage for representative
desktop and phone routes, including Sessions composition, terminal splitting,
zoom-event ownership and dirty-editor lifecycle. A human/device M13 demo is
still outstanding and `ROADMAP.md` says so.

---

## 1. Getting in

> The addresses in this section are **the maintainer's own deployment**, used
> here so the walkthrough has something concrete to point at. Substitute your
> own instance throughout. If you do not have one yet,
> [`GETTING_STARTED.md`](GETTING_STARTED.md) builds one, and
> [`DEPLOYMENT.md`](DEPLOYMENT.md) is the reference customisation these
> addresses belong to.

1. Open **https://vogt.sprooty.com** — the merged product: Vogt's work
   surfaces and the session engine's terminals on one address.
   (Until the standalone stack is retired, https://mydevenv2.sprooty.com still
   answers and serves the session engine *without* Vogt — terminals and the
   assistant work, and every Vogt surface reports that no core is configured.)
2. Open **Settings** — from the desktop Places rail's footer, or on a phone
   from the bottom bar's **More** sheet — and paste your bearer token.
3. **Save & reload.**

The token is a front-door token. What it can do is exactly what its
capabilities list says: reading needs only a valid token, and each kind of
write needs its own capability (`sessions`, `filesystem-write`, `git-write`,
`gui-control`, `agent-tasks-write`, `push-write`, `history-write`,
`assistant`, `vogt-write`). A token that authenticates but lacks the capability
gets a `403` — which means *this credential will never work for this route*,
as against a `401`'s *try a different one*.

Settings is a routed modal. Whether it is opened from the desktop rail, the
phone bottom bar's **More** sheet, or the command palette, closing it returns
to the route that invoked it,
including filters in the query string; a direct `#/settings` link falls back
to Sessions. Browser Back closes a routed Settings view without adding a loop.

Settings stores device-local **named auth profiles**, so you can keep a
read-only token and an interactive one side by side and switch between them
rather than leaving an admin token in a browser.

## 2. The surfaces

Board, Backlog, Inbox, and Sessions share one working-header order: the
surface title, the freshness/evidence or connection truth that qualifies its
answer, view controls, then the primary action. A surface can omit a control
or action, but it does not move the truth statement away from the answer it
qualifies. On narrow screens and at increased browser zoom those regions wrap
in the same reading and keyboard order. Longer implementation detail is
available from the header's disclosure where a surface needs it; unavailable,
empty, loading, and partial answers remain named in the surface's own words.

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
compact header keeps **New file** and **Refresh** immediate, while **New
folder** and **Upload files** sit under **More file actions**. Each row keeps
the name first, folders disclose nested rows with a caret, Git-changed files
show a quiet status letter such as `M`, and the labelled row menu retains
rename/move, duplicate, download, delete, upload-here, terminal and preset
operations where they apply.

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
| **History** | `#/history` | Archived scrollback from sessions that have ended, full-text searchable. |
| **Assistant** | `#/assistant` | The conversational surface — absent entirely unless the deployment provisioned it. |
| **GUI** | `#/gui` | Hidden unless the server reports a configured stream, a shipped Selkies feature and an operator-recorded end-to-end verification. An old/direct link otherwise explains that the surface is unavailable. |

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
  deliberately not built — `REQUIREMENTS.md` §3). Opening an item and coming
  back keeps the filter set — through Browser Back, and through the rail,
  command palette and phone bottom bar, which return to the last filtered view
  the surface held rather than a bare board.
- **Names, not refs.** A card names its project and its assignee rather than
  showing the raw slug or identity ref, resolving each through the lists the
  board already loaded and keeping the raw ref in the element's tooltip when
  they differ; the filter chips name the project and assignee the same way. The
  project on a card is a link to its Projects page.
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
a form-validation quirk; it is the product's rule (FR-W1). The reason is what
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

With no tool open, the Sessions overview is a live list of the running
sessions — name, activity dot and state word, and working directory — each a
link into its terminal, sorted with whatever wants attention first. When there
are no sessions at all it becomes a **Start a session** button beside the
configured presets, rather than an empty panel.

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

Git status, branches, commit history and the selected diff report failures in
their own panels and offer Retry. If a refresh fails after a successful read,
the previous answer remains visible but is explicitly marked stale. A clean
working tree, a repository with no branches or commits, a selected path that
is not a repository, and a missing/unavailable path are therefore separate
states rather than different ways to draw an empty panel.

History loads archived sessions in pages and says how many are loaded versus
the server-reported total when that count is available. If no total is
available, a full page says that more may exist until **Load more** reaches a
short page. Metadata filters apply only to the pages already loaded; archived
output search runs across the server's full archive (up to the displayed result
limit) and is debounced, so typing a needle settles to a single server search
rather than one per keystroke. A failed archive, search, detail or replay read stays attached to that
panel with **Retry**. Previously loaded content remains visible but is marked
stale; an empty archive is shown only after a successful empty response.

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

### 2.7 Scheduled agent tasks

A task is a command, a prompt, a schedule (`manual`, `interval`, or UTC
`daily`) and a persistent context file. Running one spawns a real PTY session,
so attach, scrollback, history, auth and push all work the way they do
everywhere else.

Task edits remain a draft until **Save Changes** or **Create Task** succeeds.
Selecting another task, starting a new draft, refreshing, closing the Tasks
tab, or leaving the route asks whether to save, discard, or remain; a failed
save keeps every field. An unavailable first read is shown as an error rather
than as an empty task list. If a refresh fails after a successful read, the
last task list stays visible, is marked stale, and can be retried in place.

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
- **A session waiting for input becomes a card**, above the roster, showing
  the prompt it is actually waiting on. **Send y + Enter** and **Send Ctrl-C**
  send those keystrokes to that session's terminal — they are terminal input,
  not Vogt approvals, and the card says so. If the prompt cannot be read, or
  the session has exited, the card says that instead and offers nothing to
  press. Below the cards, the running sessions that are *not* waiting still
  list as rows, so an idle or busy session is one tap from its terminal.
- **Opening a terminal or tool collapses the Sessions header.** On the
  overview the header is the surface's own — kicker, title, the connection
  line and the tool strip. Once a terminal, History, Git, Assistant or Tasks
  owns the screen, that header folds to a single row (the title and **+
  Session**), the tool strip becomes one scrolling row, and the connection
  line moves behind **View controls** — so the terminal keeps at least 40% of
  the screen instead of being pushed off the bottom.
- **The bottom bar reaches four places** — Sessions, Inbox, Board, Backlog —
  and a fifth **More** slot opens a sheet with the rest (Projects, Audit, Git,
  History, Tasks, and GUI stream and Assistant where those are enabled) plus
  **Settings** and **Sign out**. Every place and both account actions are two
  taps from any surface. The bar lifts above the soft keyboard rather than
  hiding under it.
- **Modifier row** above the soft keyboard: `Esc`, `Tab`, sticky `Ctrl`,
  arrows, `/ | ~`, `Enter`. Tap `Ctrl` then a letter to send `Ctrl+letter`.
- Use the terminal's labelled **A− / A+** controls for terminal-only font size;
  the size is remembered per device. Browser pinch/Ctrl+wheel zoom remains the
  normal whole-app zoom gesture.
- **Copy/paste**: long-press to select, long-press empty space to paste.
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

**The command palette reaches every read surface by fuzzy name** — projects and
work items included, so `rstnz` finds `rustnzb`. **New File** opens a form with
separate workspace destination and filename fields; **Open File…** opens a
searchable workspace-file chooser. Cancelling either returns focus to the
palette invoker, and creation writes only after **Create file** succeeds.
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
file, archived-output, or current-file symbol search. Closing or changing a
special search cancels its obsolete request. Choose **Refresh Command Palette
Data** when a write elsewhere makes a cached command list out of date.

Prefix a palette query with `>` to search archived terminal output. Choosing a
match opens a shareable History URL carrying the query, archived session and
selected excerpt; reload restores the same marked context, and Browser Back
returns to the route from which the palette was opened. The **Search History**
command opens History with its archived-output search field focused.

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

For an agent, `vogt-mcp` speaks MCP over stdio against the same data directory,
and `vogt-mcp-remote` bridges stdio to a remote instance's `/mcp`. An agent's
tool list is exactly what its token may do — ungranted tools are absent rather
than present-and-refusing — and every write it makes requires a reason and
lands an audit row carrying it. `DEPLOYMENT.md` §7 is the full story of
reaching an instance from an agent environment.
