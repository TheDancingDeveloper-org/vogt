# Vogt and ProPR — a comparison, and what is worth taking

*Read on 2026-08-16 against `integry/propr` at `d537c25` (v1.1.0), its
Docusaurus documentation set under `docs/docs/`, and this repository's
`README.md`, `docs/ENGINE.md` and the requirements gap register.*

**What this document is.** A comparison against one adjacent product, and a
ranked list of features it has that Vogt could adopt. It is **not a backlog and
nothing in it is scheduled** — the same rule the requirements gap register
states for itself. An item here becomes work when it becomes a numbered
requirement, and not before. Where an item touches a requirement that already
exists, or a decision already taken, this document says so by ID rather than
re-opening it.

**Freshness.** Every claim about ProPR below was read from that commit's
documentation or source on that date. Every claim about Vogt was read from this
tree on the same day. Neither is re-checked on a timer, which is the same
contract everything else in this repository offers.

---

## 1. What the two products are

They are not competitors. They occupy adjacent halves of the same problem and
overlap in one place.

**Vogt is a tracker with a doing-surface.** Projects, backlog, bugs, ranking,
drift, contract, audit, and — since the session-engine merge — PTY sessions bound to
work items. Its centre of gravity is *knowing what is true, with provenance and
freshness*. Its principles are in `README.md`: observed-first, reports never
enforces, never goes looking, declared vs observed always separated, every
answer carries provenance, transport parity, writes are first-class,
forge-optional, MCP by default.

**ProPR is a pipeline with no tracker.** It turns a GitHub issue into a
reviewed, merged pull request, deterministically and repeatedly — its README
claims 2,100+ merged PRs since May 2025, 690+ in its own repository. It has no
backlog of its own, no ranking, no drift, no contract, no audit-with-reason, no
MCP surface, and no declared-versus-observed distinction. GitHub issues, PRs and
labels **are** its data model and its state machine.

The overlap is the session engine. Everything worth importing sits there.

### 1.1 Feature-by-feature

| Capability | Vogt | ProPR |
|---|---|---|
| Backlog with explainable ranking | Yes (FR-V*) | No |
| Cross-project rollup | Yes | No — per-repository stats only |
| Declared vs observed, drift | Yes (M3) | No |
| Contract checks | Yes (M3) | No |
| Audit with a mandatory reason | Yes, every write | No |
| MCP server | Yes, generated from the registry | No |
| Transport parity (CLI/REST/MCP/GUI) | Yes, asserted by test | No — UI and CLI are separate clients |
| Works with no forge at all | Yes (principle 8) | No — a GitHub App is required |
| Terminal sessions, watchable/steerable | Yes (FR-E1, FR-E2) | No — logs and streamed output only |
| Scheduled agent runs | Yes (FR-E7: manual/interval/daily) | No |
| Per-run git worktree isolation | **No** | Yes |
| Branch + commit + push + open PR from a run | **No** | Yes |
| Cost and token accounting | **No** | Yes, per call and aggregated |
| Provider capacity/rate-limit visibility | **No** | Yes, via optional Agent Tank |
| Multi-model fan-out on one item | **No** | Yes, one PR per `llm-*` label |
| Plan-before-execute drafting surface | **No** | Yes (Planner Studio) |
| Repository indexing and summaries | Partial — collectors, not summaries | Yes |
| Scored, structured AI review | **No** | Yes (`/review`, `Score: N/10`) |
| Queue with retry, backoff, correlation IDs | **No** | Yes (Redis/BullMQ) |

---

## 2. How ProPR works, in enough detail to judge the imports

**The three-phase pipeline.** Worker execution splits into phases and the agent
participates only in the middle one:

1. **Pre-agent setup (ProPR):** pull the job from the queue, update the base
   branch, create an isolated git worktree, create the task branch, prepare the
   prompt and context.
2. **Agent implementation (agent):** run the selected agent CLI inside a
   container against that worktree. The agent edits files; it does not push,
   branch, or open pull requests.
3. **Post-agent finalisation (ProPR):** inspect changed files, commit, push,
   create or update the PR with issue linking, update labels and task state.

Because phases 1 and 3 are deterministic code rather than agent decisions, a
failure is attributable: phase 1 or 3 is a git/GitHub problem, phase 2 is an
agent problem. Branches are named
`<issueId>/<model>-<sanitized-title>-<YYYYMMDD-HHMM>-<random>`.

**Intake** is a GitHub label. An issue tagged with a trigger label (`AI`,
`propr`) plus `llm-<agent>-<model>` routes to that agent; several model labels
fan out to one job, worktree, branch and PR *per model*.

**Follow-up** stays in the pull request. Plain comments are picked up as
instructions (line-level ones carry file, line and diff hunk; attached images
reach the agent; comments arriving during a running job are batched). Slash
commands cover the rest: `/review` (read-only, severity-tagged findings and a
`Score: N/10` line), `/fix` (applies the review suggestions a human left
standing), `/merge`, `/switch`, `/use`, and `/ultrafix` (alternates review and
fix until the score reaches a goal, with a PR label as circuit breaker).

**Planner Studio** is the plan-first entry point: Define & Context (repository,
model, context level, attachments, a context preview and an estimated issue
count *before* generation) → Review Plan (edit, delete, restore, undo/redo,
refinement chat) → Execution (per-issue status, pause/resume, links to the
resulting PRs). Finalising creates GitHub issues.

**Repository knowledge** indexes monitored repositories into file and
repository summaries, with a fallback model and quota-aware cooldown so
indexing degrades rather than fails; a background worker rescans every 5
minutes and reindexes daily. On top of the index sit repository chat,
categorised improvement suggestions, and repository todos.

**Observability** is a task record (trigger, repository, branch, agent, model,
state, commits, PR, failure point), the exact prompt and logs, a completion
comment posted back to the PR, and a metrics trail: every model call lands in
an `llm_logs` table with execution type, model, agent alias, input/output and
cache token counts, a USD estimate, duration and correlation ID, aggregated per
repository, per model and per day, with a high-cost alert above
`LLM_COST_THRESHOLD_USD`.

**Agent Tank** is a separate optional local service that reads each agent CLI's
own `/usage` output for subscription capacity, surfaced as usage bars and a
per-call usage delta. It is explicitly best-effort: unreachable means the bars
hide and the work completes normally.

---

## 3. Tier 1 — worth building, and each closes something already owed

### 3.1 One git worktree and branch per run

ProPR reuses one clone per repository and creates a worktree per task; branch
names carry the model identifier so concurrent runs never collide.

Vogt opens a session **in the project's registered root path**, and
`engine/server/src/git.rs` operates on that checkout directly. That is exactly
the hole gap **FR-E11** names: *"Two agents can edit one
checkout concurrently and neither is told. No audit row records the loss,
because both writes are legitimate."*

Worktree-per-session does not *detect* the collision — it makes it impossible.
This is the highest-value single import in this document: it closes an owed
`could-have` by construction, and items 3.2 and 3.4 depend on it.

### 3.2 A deterministic post-agent phase

Vogt's session lifecycle ends at *PTY exited, exit code recorded, findings
appended* (`ENGINE.md` §7). A finalisation phase — inspect changed files,
commit, push, open a PR linked to the work item, transition the item — turns
"WI-7 had a session" into "WI-7 produced a reviewable change."

It sits inside M5's existing rule that forge write-back is **additive and
forward-only**: opening a pull request adds and destroys nothing. It also makes
failures phase-attributable, which run records cannot do today — they carry
`exit_code` and a derived summary, not a failing phase.

### 3.3 Cost and token accounting

Vogt has no concept of what work costs. This is the most natural import on the
list, because it is already a Vogt-shaped sentence: *"WI-12 has cost $34 across
6 sessions, last observed 4 minutes ago."* Cost is an observation with a source
and an age, exactly like everything else here.

Two consequences beyond the number itself: it makes an expensive loop visible
before the bill does, and — uniquely among the items in this document — a cost
observation can enter the ranking weights.

Note ProPR's own honesty about the limits, which any Vogt version inherits: the
figure is an *estimate* from token counts and published per-model rates, with
OpenRouter as a fallback feed; provider options that change the rate but are
not reported by the CLI are not included; token counts are null for some
agents. That is a freshness-and-trust problem, and this product has a vocabulary
for it.

### 3.4 Multi-agent fan-out on one work item

Several `llm-*` labels on one ProPR issue produce one run, worktree, branch and
PR per model; you compare, merge one, close the rest.

Vogt already has labels, work items and a session registry. With 3.1 in place
this is mostly wiring, and it is differentiated for a *tracker*: comparing
several implementations of one unit is a ranking problem, and ranking is what
this product does.

---

## 4. Tier 2 — good fit, more design work

### 4.1 Repository knowledge as a collector

ProPR's indexing, summaries, repository chat and improvement suggestions map
onto machinery Vogt already has. A `repo-summary` collector produces
**observations** with freshness and trust; improvement suggestions land as
**collected subjects in the ranked backlog**, visible by default and
authoritative only once adopted (principle 1). Nothing about it needs a new
architectural concept, and the quota-aware fallback is a good pattern to copy
verbatim: degrade the model, keep the sweep moving.

### 4.2 A planning surface that fans one request into several work items

Planner Studio's shape — draft, preview the context and cost *before*
generating, review and edit, then commit — is a checkpoint Vogt does not have.
Vogt has work items, relations and briefs, but no drafting-then-committing
surface.

**Check this against a closed decision before designing it.** §7.3 records
"natural-language task drafting" as **withdrawn**, but that withdrawal was
about *agent tasks*, where interval and daily schedules covered the motivating
cases. Fanning one goal into several related, reviewable work items is a
different thing. It should be adopted or refused explicitly, not inherited by
resemblance in either direction.

### 4.3 Review as a scored observation

`/review` emits a fixed structure — severity-tagged findings (🔴 Critical, 🟡
Warning, 🟢 Suggestion, ✅ Positive) and a closing `Score: N/10` — and touches
no files; `/fix` then applies only what a human left standing. The reusable
part is the *structured, scored, read-only* output: a review score with a date
and a source is an observation, and it gives the backlog a quality signal it
does not currently have.

The human-edits-between-review-and-fix step is worth keeping too. It is the
same shape as the assistant's on-screen approval (FR-T*): the machine proposes,
a person prunes, and only then does anything change.

### 4.4 Provider capacity as an observation

Agent Tank reads each CLI's own reported limits — nothing scraped, nothing
estimated — and ProPR degrades to "no capacity bars" when it is absent. That
best-effort contract is already this product's default posture. Cheap to add,
and it makes *"why did nothing run last night"* answerable.

---

## 5. Tier 3 — take the mechanism, not the feature

- **Queue mechanics.** Correlation IDs traced across daemon, worker and agent;
  exponential backoff with jitter on transient git and GitHub failures; comment
  batching while a job for the same subject runs; a worker concurrency cap;
  queue-depth statistics. Vogt's scheduler is `manual`/`interval`/`daily` with
  no queue, no concurrency limit and no retry. **Take the ideas, not Redis** —
  a broker dependency breaks the single-node zero-dependency SQLite
  self-hosting story that `README.md` puts in the stack line.
- **Comment-driven intake.** Vogt already collects GitHub issues and PRs
  read-only (M5) and consolidates them at import (M7); treating a comment as
  *intake* is a small extension of the same adapter. It carries a real tension
  with `ENGINE.md` §8's *"it never decides to run anything"*. The argument that
  a person's comment is a person's act is available and probably correct — but
  it has to be made in the requirement, not discovered afterwards.
- **Signed system tasks.** ProPR authorises reverts with `SYSTEM_TASK_SECRET`
  so a destructive operation cannot be injected through ordinary intake. Worth
  copying for any future destructive Vogt write.
- **Author gating on intake.** Bot accounts ignored by construction, plus an
  allowlist and a blocklist. Any comment-driven path needs the equivalent
  before it needs anything else.

---

## 6. What not to take, and why

- **GitHub as the substrate.** ProPR cannot start without a GitHub App; issues,
  PRs and labels are its database. That is the direct negation of principle 8
  (forge-optional), which is load-bearing here — plain folders and local git
  are first-class in Vogt and the GitHub adapter only ever *adds*.
- **Label-triggered autonomous pickup.** ProPR's entire intake is "label an
  issue and an agent starts." Autonomous work pickup is deferred by name as
  a non-requirement, and named again in `ENGINE.md` §8 as the surviving core
  of a reversed non-goal. Adopt the *routing* idea — a label selects agent and
  model — without the *triggering* idea.
- **Authoritative state living in forge labels.** ProPR's
  `<trigger>-processing` → `<trigger>-done` → `<trigger>-failed-*` lifecycle is
  precisely the pattern Vogt's `forge_state_mismatch` drift exists to catch.
  Anything imported must arrive as observations with freshness and trust, or as
  audited writes with a reason — never as a second state machine living
  upstream.
- **Docker-socket-per-agent-run.** A container per run per agent image, with
  direct socket access; their own hardening firewall ships disabled because
  applying it would need `--privileged`. Vogt's PTY sessions are lighter and
  better audited.
- **Redis**, per §5 above.

---

## 7. The short answer

If exactly one thing is taken: **worktree-per-session (§3.1), plus the
deterministic finalisation phase (§3.2)**. Together they close FR-E11 by
construction and turn a Vogt work item from a card sitting next to a terminal
into a card that produces a pull request — which is the whole of what ProPR has
demonstrated works at volume.

If a second: **cost accounting (§3.3)**, because it is the only item here that
can feed the ranking.
