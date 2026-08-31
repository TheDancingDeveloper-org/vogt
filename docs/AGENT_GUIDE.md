# Vogt — Agent Guide

You are an agent, and this guide is for you. It is not about developing Vogt —
[`../AGENTS.md`](../AGENTS.md), [`../engine/AGENTS.md`](../engine/AGENTS.md) and
[`CONTRIBUTING.md`](CONTRIBUTING.md) cover working *on* the product. This guide
is about running a stream of product work *through* Vogt: picking up a piece of
work, doing it in a checkout, and leaving behind a record that a person — or the
next agent — can read and trust.

Vogt is a work register, not a task runner. It **reports, it does not enforce**:
it tells you what is true, how old the answer is, and who last touched it, and
then it lets you decide. Nothing here takes compliance, trust, or drift as a
precondition for an operation. Read that as a promise and a constraint — Vogt
will not stop you doing the wrong thing, so the discipline is yours.

Everything below is reachable three ways from a terminal or an agent runtime —
CLI, REST, and MCP — generated from one operation registry, so a verb named on
one surface exists on the others too. Where this guide writes an MCP tool it
uses the underscore form (`work_get`); the same operation is `work get` on the
CLI and `work.get` in the registry and audit log. The browser GUI and the voice
assistant, served by the optional engine, drive the very same operations; if you
are working through those, [`USER_GUIDE.md`](USER_GUIDE.md) is your reference and
the rules here still hold.

Status: current as of 2026-08-23.

---

## 1. Connecting

Ask the instance how to reach it before guessing. `vogt connect` renders the
address and a ready-to-paste client configuration; it is a read, so it needs
only a valid token.

```console
$ uv run vogt connect                 # HTTP/MCP client config for this instance
$ uv run vogt connect --client bridge # the stdio-bridge config instead
```

### The three surfaces

| Surface | How you reach it | Use it when |
|---|---|---|
| **MCP** | `vogt-mcp` speaks MCP over stdio against a local data directory (`VOGT_DATA_DIR`); `vogt-mcp-remote` bridges stdio to a running instance's `/mcp` using `VOGT_URL` and `VOGT_TOKEN_FILE`. A client that speaks streamable HTTP can talk to `/mcp` directly. | You are an agent with an MCP client. This is the primary surface. |
| **REST** | The same operations are routes under `/api/`. Send your token as `Authorization: Bearer …`; `GET /openapi.json` lists every route with its request and response schema. | You are scripting, or your runtime has no MCP client. |
| **CLI** | `uv run vogt <verb>` (or the installed `vogt`), against the local data directory or a configured instance. | You are working in a shell in the checkout. |

Your MCP tool list is exactly what your token may do: an operation your token
cannot reach is **absent from the list**, not present-and-refusing. So if a tool
you expected is missing, that is a scope answer, not a bug — see below.

### Token scopes

A core token is issued with `vogt token issue` and carries named scopes. Reading
needs `read`; each family of writes needs its own scope, so a read-only token can
list and explain work but not move it.

| Scope | Grants |
|---|---|
| `read` | Every read: `backlog`, `bugs`, `why`, `work_get`, `project_brief`, `audit_list`, … |
| `work.write` | Work-item writes: `work_create`, `work_transition`, `work_comment`, `work_relate`, `work_adopt`, `suppress`, … |
| `project.write` | Project writes: `project_register`, `project_create`, `contract_adopt`, `initiative_create`, `initiative_publish`, … |

Ask your operator for a token scoped to the work you will actually do; do not ask
for more. (If you are driving Vogt *through the engine's* browser or agent-task
surfaces, those carry a separate capability list — `sessions`, `vogt-write`,
`agent-tasks-write`, and so on — and the engine performs each Vogt write with the
**core token paired to yours**, so it is audited to your actor, never a shared
one. [`USER_GUIDE.md`](USER_GUIDE.md) §1 has that table.)

### The `reason` rule — every write carries one

Every mutating operation takes a required, non-empty `reason`. This is not form
validation you can pad past; it is enforced at the point the registry is built —
a write defined without a required reason fails to load at all (FR-S1). The
reason is the one caller-supplied field that lands in the audit row, so it is
the sentence a person reads six weeks later to understand what you did.

Write the reason you would want to read. "triage" armed once and fired across
fifty transitions is how an audit log becomes noise. Say *why this write, now* —
"closing: merged in acme-web#42", not "update". Every declared write lands the
entity change, the audit row and the event row in one transaction, so the reason
and the effect can never drift apart.

---

## 2. Picking up work

### Read the ranked views

`backlog` and `bugs` are the ranked global views — declared work and observed
subjects ranked together, newest evidence lifting an item and idle work sinking.
They take the same filters (project, label, initiative, actor, kind) so you can
scope to your lane.

```console
$ uv run vogt backlog --project acme-web
$ uv run vogt bugs --project acme-web
```

Every row can explain itself. `why` is not a summary — it is the ranking
function's own account of where an item sits and what moved it there.

```console
$ uv run vogt why --ref WI-42
```

### Read provenance, trust and age before you act

A Vogt answer is never just a value; it carries how it was learned and how old
that is. Read those three signals before you treat an answer as true:

- **Declared vs observed.** Declared facts are what a person (or you) asserted;
  observed facts are what a collector found in a checkout or on a forge. They are
  kept separate on purpose — a disagreement between them is surfaced as *drift*,
  never silently reconciled.
- **Trust and verification.** An observation whose payload does not claim to have
  been verified reads as `unverified`, not blank — a blank would say "no opinion"
  when the honest answer is "nobody checked". A claim backed by a **still-running**
  session is `provisional`, not fresh.
- **Age.** Most collectors run on a schedule and their answers go stale between
  sweeps. Anything that aggregates says how old its answer is. A well-formed
  answer is not automatically a current one.

If you are about to act on a fact, check that it is fresh and trusted enough for
what you are about to do. Acting on a three-week-old, unverified observation as
though it were checked this morning is the most common way an agent does the
wrong thing confidently.

### Open the item in full

`work_get` returns one item completely: description, state history, comments,
typed relations, the labels and evidence collected against it — and its derived
**git story** (§3). That is where you learn the item's context before you touch
it: what it depends on, what depends on it, which initiative it belongs to.

```console
$ uv run vogt work get --ref WI-42
```

If the item belongs to an initiative, read that too (`initiative_list`) — a
cross-project epic changes what "done" means for one member.

---

## 3. Doing the work

### Start from the item, not from a bare shell

A session is how a checkout gets bound to a work item. `session_start` (available
when the engine is running) opens a PTY in the project's registered working tree
and writes the item's brief — its description, its `why`, its relations — to a
prompt file the agent is pointed at, so you begin with the item's context rather
than reconstructing it.

```console
$ uv run vogt session start --work-item WI-42 --reason "start on the login-timeout fix"
```

The session is linked back to the item: the item shows a live-activity badge
while you work, and the session's findings become collectable evidence against
the item. If you are running as a **scheduled agent task** instead, bind the task
to its subject with `vogt_work_item` (a ref like `WI-42`) or `vogt_project` (a
slug) so the run's findings file as observations against that subject with the
same freshness and trust every other kind of evidence carries.

### Search session history — live and archived

Vogt exposes the engine's session history as three reads, so you can find what
any session has printed without leaving the tool surface (they need only a
running engine; no capability beyond the ordinary session token):

- `session_search_output` — full-text search over session output. It covers
  **running** sessions too, not just the archive: each hit carries `live`, so a
  match in a session that is still going is distinguishable from one in a
  finished run. Pass `include_live=false` for archive-only.
- `session_log_tail` — the tail of one session's output log, readable
  (`strip_ansi` defaults on). Works for a live session as well as an archived
  one.
- `session_history_list` — the archived-session listing, newest first.

```console
$ uv run vogt session search --q "connection refused"
$ uv run vogt session log --id <session-id>
```

Each returns an `engine` field that is set (with the reason) when the engine
could not be asked; an outage reads as an empty result, never as "no history".

### Name the branch so Vogt can see it

Vogt recognises which work item a git branch belongs to by its **name**, using
the configurable `branch_binding_patterns`. The shipped defaults match a
work-item number (`wi-7`, `WI-7`, `feature/WI-7-…` — the pattern is
`(?i)\bwi-?(?P<n>\d+)\b`) and a forge issue number (`gh-264-…`, from
`(?i)\bgh-(?P<forge>\d+)\b`). Check your instance's `CONFIG.md` for the exact
set your operator configured; do not assume the defaults if the estate uses its
own prefixes.

When a session started *from Vogt* declares the branch it will use, that name
comes from `branch_binding_template`, whose shipped default is `wi-{number}` —
so `WI-42` declares branch `wi-42`. Follow that convention when you branch by
hand:

```console
$ git switch -c wi-42            # matches the default template and patterns
```

A branch whose name Vogt can bind shows up in the item's **Branches** panel,
marked **declared** (a session said it would use it), **observed** (a sweep found
it in the checkout), or **both**. A branch on one side only is marked **drift** —
Vogt reports where work is in git and never drives it, so a disagreement is shown,
not reconciled.

### Link the PR back to the item

When you open a pull request, name the item in the way Vogt already reads: a
`Closes #42` / `Fixes owner/repo#42` closing keyword in the PR title or body, or
a branch named for the issue. The forge sync reads that as an **`implemented_by`**
edge (#284) and folds the PR under the work item it implements, so a single piece
of work is counted once in the ranked views instead of ranking twice.

Two things worth knowing about that edge: it is **observed, never typed in** —
`work_relate` refuses `implemented_by` — and it **only informs**. Unlike
`depends_on`, an open `implemented_by` PR does not block the item from completing;
it feeds the item's phase and its ranking, nothing more.

### The git story that results

From the branch tips (#283) and the PR edge (#284), `work_get` derives a
read-only **git story** (#285): a **phase** — `no_branch → branch_active →
pr_open → in_review → merged` — shown *beside* the workflow state, plus the PR's
derived state, review decision and checks rollup, each stamped with how fresh
the observation is. Nothing is written back: the phase is a second opinion, so a
`merged` phase on an item still `in_progress` is exactly the disagreement it
exists to make visible, and the obvious contradictions (a PR merged while the
item is open, a branch still active for an item marked done) surface as drift.

### What Vogt observes vs what you must declare

This is the line to keep straight. Vogt **observes** git and forge state on its
own; you **declare** the meaning.

| Vogt observes (you never type these) | You declare (an explicit write, with a reason) |
|---|---|
| Branches in the checkout and how they've diverged | The work item itself: `work_create` |
| The PR, its state, reviews, checks | Its state moves: `work_transition` |
| The `implemented_by` PR→item edge, from keywords/branch names | Typed relations: `work_relate` (`depends_on`, `relates_to`, `duplicate_of`, `parent_of`) |
| CI status, source markers, dependency edges | Comments, adoptions, suppressions: `work_comment`, `work_adopt`, `suppress` |

Vogt will observe your branch and your PR without being told. It will **not**
move the work item to `in_progress` or `done` for you — that is a declaration
only you can make, and §4 is about making it well.

---

## 4. Moving work

### Transition with a reason

`work_transition` moves an item to another state, validating the edge against the
workflow. The workflow is the server's, not yours: an illegal edge is refused by
name, and the reason you give is written to the audit row.

```console
$ uv run vogt work transition --ref WI-42 --to-state in_progress --reason "picked up: starting the fix"
```

### What blocks, and what does not

Only one relation gates completion. An item with unfinished **`depends_on`**
targets cannot move to `done`; the refusal names the rule and lists the blockers,
so you can act without a second query (FR-W8). Everything else — trust,
compliance, drift, an open `implemented_by` PR — **never** gates an operation.
That is the "reports, never enforces" promise in force: Vogt will let you close
an item whose PR is still open, and simply record the contradiction as drift.

### Comment, adopt, suppress

- **Comment** (`work_comment`) when you need to leave a note on the item — a
  decision, a blocker, a hand-off — attributed to you.
- **Adopt** (`work_adopt`) an observed subject when it is real work worth
  declaring: adoption upgrades an observed marker into a first-class item.
  Observed-first means it was already visible; adopting it raises its trust.
- **Suppress** (`suppress`) an observed subject that is noise — with a reason,
  because suppression is a decision someone may later want to understand.

### Initiatives and `initiative.publish`

An initiative is a cross-project epic that lives in Vogt. To make it visible to
people who only have the forge open, `initiative_publish` creates — or, on a
later run, re-adopts — **one tracking issue per forge-linked project** the
initiative spans, carrying a checkbox list of its member work items.

```console
$ uv run vogt initiative publish --slug platform-epic --reason "make the epic visible on the forge"
```

It is safe to run again and again, and the reasons are worth internalising: it
only ever rewrites the region between its `<!-- vogt:initiative:… -->` markers
(anything you write outside them is preserved); it **adopts, it does not
duplicate** (the hidden marker is how the next run finds the issue it opened);
it **never closes** a tracking issue — when the initiative closes, publishing
*proposes* the close as a drift proposal for a person to resolve; and a tick
made upstream while the member is still open surfaces as `initiative_checkbox_drift`,
not a silent overwrite. Publishing writes to the forge, so a project needs
`forge_writeback` set to `full` and a linked repository (`forge_link`) first; a
repo it touches that is not linked is reported as skipped, with the reason.

---

## 5. Explaining yourself — check your effect, not your intent

You know what you meant to do. The audit log knows what you did. When you want to
confirm a stream of work landed the way you think it did, read the record, not
your own memory of it.

- **`why`** tells you where an item sits and what moved it — run it after a
  transition to confirm the ranking reflects reality.
- **`audit_list`** is every write: who, what, and the reason. Filter it by actor,
  project, operation or time to see exactly the rows your work produced. This is
  where a hollow reason ("update", "fix") reads back as exactly as unhelpful as
  it was to write.
- **`drift_list`** is where your effect and the world's disagree — a PR you
  merged while the item is still open, a branch still active for an item you
  marked done, a checkbox ticked on the forge. Drift is not a failure; it is the
  system telling you the two registers no longer agree, so you can decide which
  is right. Resolving drift (accept or reject) is itself a declared write with a
  reason; there is deliberately no bulk-accept.

The habit to build: after a piece of work, ask Vogt what it now believes, and
reconcile that against what you intended. Checking your *effect* rather than your
*intent* is the whole point of an observed-first register.

---

## 6. Boundaries — what Vogt will and will not do

These are not limitations to route around; they are the shape of the tool.

- **Write-back is additive and forward-only.** Vogt's forge writes append —
  create an issue, add a comment, add a label, publish a tracking region. There
  is **no force, ever** (FR-B4): Vogt does not delete branches, does not delete
  or force-push repositories, and does not edit an upstream issue's title or body
  on a linked project — that text is upstream truth Vogt reads, not owns.
- **Nothing enforces.** Compliance, trust, and drift are values to read, never
  preconditions. The one gate in the whole model is `depends_on` blocking
  completion, and that is the declared meaning of an edge you created yourself.
- **Nothing discovers.** Collection scope is the registered project list. Vogt
  does not crawl filesystem roots or list candidate repositories; if you want a
  repository observed, it must be registered first.
- **Approval gates fail closed** (#289). When you run work as a scheduled agent
  task, a declared gate holds the PTY until a person answers, and a gate that is
  interrupted, times out, or whose session dies resolves to **blocked** — never
  to an approval. An interruption is not a yes. The only bypass is a task
  explicitly set to auto-approve, and that is recorded as such on the gate.
- **Vogt never decides to run anything.** Every session and every run traces to a
  person or to a schedule a person created; there is no autonomous work pickup.

Read those as the reasons you can trust the record: because Vogt cannot quietly
rewrite git history or an upstream issue, what it reports is what happened.

---

## 7. A worked example

One work item, from the backlog to a merged PR, as a reproducible transcript.
The refs are illustrative — a project `acme-web`, work item `WI-42`, forge issue
`#42` — but every command and every behaviour is real.

```console
# 1. Find work and understand it.
$ uv run vogt backlog --project acme-web
  #1  WI-42  login times out after 30s   trust=declared  age=2d  score=8.1
$ uv run vogt why --ref WI-42
  WI-42 ranks #1 in acme-web: p1, no branch yet, unblocked, 2 days idle …
$ uv run vogt work get --ref WI-42
  … description, relations (depends_on: none), git story phase = no_branch …

# 2. Claim it. The transition is a declaration, with a reason.
$ uv run vogt work transition --ref WI-42 --to-state in_progress \
    --reason "picked up: reproducing the 30s auth timeout"

# 3. Open a session bound to the item (writes the item's brief to a prompt file).
$ uv run vogt session start --work-item WI-42 \
    --reason "work the login-timeout fix in the acme-web checkout"

# 4. Branch by the convention Vogt binds (default template wi-{number}).
$ git switch -c wi-42
  # … do the work, commit …
$ git commit -m "fix: cap the auth handshake at 10s

Closes #42"

# 5. Open the PR with a closing keyword so Vogt reads the implemented_by edge.
#    (branch wi-42 + 'Closes #42' → the PR folds under WI-42 in the ranked views)
$ gh pr create --fill

# 6. A sweep observes the branch and the PR; the git story climbs on its own.
$ uv run vogt work get --ref WI-42
  … git story: phase = pr_open → in_review → merged, checks green (observed 4m ago) …

# 7. When the PR is merged, close the item — a declaration only you make.
$ uv run vogt work transition --ref WI-42 --to-state done \
    --reason "closing: merged in acme-web#42"

# 8. Check your effect, not your intent.
$ uv run vogt audit list --ref WI-42     # your three writes, each with its reason
$ uv run vogt drift list --project acme-web   # confirm no item/PR disagreement lingers
```

Notice what you never did: you never told Vogt about the branch or the PR — it
observed both — and you never forced anything. You declared three things (in
progress, the relation-free context, done), each with a reason, and let Vogt
observe the rest. That is the whole loop.

---

## Drop-in template for your own repository

To point the agents working in *your* repository at Vogt the same way, copy the
short block in [`agent-guide-template.md`](agent-guide-template.md) into your
project's `AGENTS.md` or `CLAUDE.md` and fill in the two bracketed values. It is
reproduced here so you can see what it says:

```markdown
## Working through Vogt

This repository is registered in Vogt (`<your-project-slug>`), the work register
for this estate. Run product work *through* it:

- **Pick up work** with `backlog` / `bugs`, and read `why <ref>` before acting.
  Check each answer's provenance, trust and age — an unverified or stale
  observation is not a checked fact.
- **Claim it** with `work transition <ref> --to-state in_progress` and a real
  reason. Every write needs a reason; it lands in the audit log.
- **Branch** so Vogt can bind it: `wi-<n>` for work item WI-<n> (the default
  `branch_binding_template`; check this instance's CONFIG.md if the estate uses
  its own prefixes).
- **Link the PR back** with `Closes #<n>` in the title or body — Vogt reads the
  `implemented_by` edge and folds the PR under the item. It informs; it does not
  block completion.
- **Close it** with `work transition <ref> --to-state done` once the PR is
  merged. Only `depends_on` blocks completion; nothing else gates.
- **Vogt observes** branches, PRs, CI and drift on its own — you never type those
  in. **You declare** the work item, its state, its relations and its comments.
- Reach Vogt at `<instance-url>` via MCP (`vogt-mcp-remote`), REST (`/api/`,
  bearer token) or the `vogt` CLI. Run `vogt connect` for the exact client config.
```

See [`AGENT_GUIDE.md`](AGENT_GUIDE.md) — this guide — for the reasoning behind
each line.
