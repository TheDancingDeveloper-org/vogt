# Agent harness services: Vogt owns the rules, the CLIs stay thin

Design for WI-1317 (2026-10-13). Challenge of oversight's draft in
`aidevenv/docs/reviews/2026-10-13-vogt-harness-services-brief.md`, written
against the engine and core on `integrate/vogt-next` at `99355800`, the
installed helpers in `/usr/local/bin/vogt-*`, msp-klaudia `be8cd0b`, and work
items WI-1279, WI-1281, WI-1291 and WI-1298 to WI-1316.

The initiative that carries the work is `agent-harness-services`. The item
refs in §9 are the Vogt items created from this doc (WI-1318 to WI-1338,
plus the existing items mapped in).

## 0. Summary

Oversight proposed six services. This design keeps four of them, merges two
into existing primitives, and drops one proxy outright:

| Oversight's draft | Here | Why |
| --- | --- | --- |
| 1. Workspace service | **Kept**, as the *workspace* half of a session: `session.start(worktree=…)` plus a workspace record the engine owns, two ops (`workspace.list`, `workspace.clean`), disk status on `engine.status`. No `workspace_create`: a worktree with no session has no owner and nothing to clean it. | WI-1298 and WI-1304 already cut most of it. |
| 2. Change and review gate | **Split.** Verdicts and verifications become *comment kinds* (records). Landing becomes one op, `work.land`, that refuses without a verdict at the head and never writes a merge commit. "Push and PR through Vogt" is **dropped as a proxy**: the policy-check route the hook already calls gains a branch-ownership rule, and org tokens are grants. | A proxy for `git push` would re-implement git credential handling for four CLIs; the hook plus a rule is one engine function. |
| 3. Attention and decision inbox | **Split.** Attention correctness is adapter work (§6) on the engine's existing title and screen contract. Decisions become a small new record, `Decision`, modelled on `SessionGrant` (request → Inbox → person answers), with the answer typed back into the asking session. | The grant flow is the one person-only decision path that already works end to end from the phone. |
| 4. Dispatch policy as data | **Kept**, in two places: per-project `agent_policy` (landing, review, verification rules) and a deployment-level dispatch table (profiles, caps) in the preference store. Enforced at `session.start` for agent callers; a person may override with a recorded reason. | Nothing new to store; the brief composer and the hook already read `agent_policy`. |
| 5. Delivery verification | **Kept as records, not states.** `landed`, `deployed`, `verified` are a projection on `work.get`, from the integration branch, the deploy-lane collector, and a `verification` comment. The one gate: an *agent* cannot close an item on a project whose policy requires a verification record. A person can. | Changing every project's workflow is invasive; a projection plus one agent-only refusal is enough. |
| 6. CI watcher | **Kept as extensions** of `ci_watch.py` and the `forge-pr-checks` trigger: hung-run detection, a flaky register, and a *person-armed* rerun task. No autonomous rerun. | ENGINE.md §8: Vogt never decides to run anything. |

Two principles hold the whole thing together:

- **Vogt enforces and suggests; a person arms the work.** Every refusal here
  is a check on a call a session or a person made. Nothing starts, merges,
  reruns or deploys by itself.
- **One rule, one place.** A rule is either engine/core-side and
  CLI-agnostic, or it is part of the minimal adapter contract every CLI must
  meet (§6). Nothing is both. Per-CLI guards that duplicate an engine rule
  are retired once the engine rule is live (§7).

## 1. What exists and what is broken

The relevant primitives, with where each lives, so the rest of the doc can
extend them by name.

| Primitive | Where | What it does today |
| --- | --- | --- |
| `agent_policy` | `src/vogt/core/entities.py:85` `AgentPolicy`; `project.update`; rendered by `_brief.py:_policy_section`; the enforced slice sent as `SessionPolicy` by `sessions.py:_enforced_policy` | `forbidden_text`, `merge_deploys_prod`, `main_branches` are enforced; `commit_identity`, `test_recipe`, `rules[]` are guidance. Filled for vogt, codex, msp-agent, rustnzb and msp-klaudia on 2026-10-13. |
| Policy check | `engine/server/src/policy.rs:check`, route `POST /api/agent-auth/policy-check` (`api.rs:381`), called by `vogt-policy-hook` (Claude Code `PreToolUse`, Klaudia `[[hooks]]`) | Parses the shell command, refuses forbidden text on writes and `gh pr merge`/`git push` to a protected branch when `merge_deploys_prod`. Fails closed when the engine is unreachable. Not installed for Codex (requirements.toml gap) or opencode. |
| Grants | `services/grants.py`, `engine/server/src/secret_broker.rs`, `docs/design/oversight-grants.md` | One named credential, one session, until a time; person-only decision from the Inbox; applied by the engine; fetched with `vogt-agent-auth fetch`. Capability grants (milestone 2) are not built. |
| Agent-auth broker | `engine/deploy/agent-auth.sh`, ENGINE.md §9 | Manifest-constrained fetch and store, audited by name. This deployment runs `identity` mode. |
| `vogt-git-askpass` | `engine/deploy/git-askpass.sh` | Answers git's username/password prompts from `GH_TOKEN` / `GIT_AUTH_TOKEN` in the session environment. |
| Commit for a worker | `GitOpReq::Commit{session,…}` (`git.rs:396`), `session.commit_for` | Oversight commits on a worker's behalf after the C14 checklist. |
| Budgets | `engine/server/src/budget.rs` | Soft line at 80 %, hibernate at 100 %, but only once the session is at its prompt; the wall clock is per turn. An autopilot worker is re-prompted before the check lands (WI-1281). |
| Autopilot | `engine/server/src/autopilot.rs` | Nudges a `ready` autopilot session after `nudge_after`, capped; stops on `AUTOPILOT: DONE`. |
| Attention | `src/vogt/core/oversight.py:classify`, `sessions.py:sweep_sessions`, `inbox.py` (`session.blocked`, `session.attention`, `session.grant_request`) | Orders the sweep by approval → blocked → waiting → stalled. Klaudia's `RequestHostChange` prompt is not read (WI-1279, fixed on the WI-1145 branch); a blocked report never clears itself (WI-1314). |
| Briefs | `services/_brief.py`, `session.start(files, symbols, done, gates, repo_map, from_handoff)` | Renders the item, the policy and the latest hand-off. The review brief (`review_of`) is on `wi-1299-review-brief` with six blockers open. |
| CI watch | `services/ci_watch.py`, `core/ci_alerts.py`, `TriggerSpec::ForgePrChecks` (`agent_tasks.rs:308`) | Announces a bound branch's CI conclusion to the Inbox and the live session; a PR-checks trigger can fire an agent task. |
| Deploy lanes | `services/deployed.py`, `deployed.versions` | Which SHA each lane runs and which work items the undeployed commits name. |
| Hand-off | `work.comment kind=handoff` with `HandoffFields` | The structured record a fresh session starts from. |
| Parity | `src/vogt/registry/engine_routes.py`, `tests/test_engine_parity.py` | Every engine route has an MCP counterpart or a written reason. Every new route below must be added to one of its two tables. |

What broke (brief §Evidence, condensed): sessions start in the shared dirty
checkout (1, 2, 3); a person-only prompt reads as idle and a stale block
never clears (4, 5); trail-off turns are not re-driven for Claude (6);
budgets are recorded, not enforced (7); worktree build output filled the disk
(8); CI hid failures and hung (9); oversight's shell merges produced merge
commits that `main` cannot take (10); three MCP footguns (11); reviewers
trusted hand-off claims (12); "done" was claimed on rollout alone (13);
approvals do not carry across CLIs and the operator gates every merge and
push (14, 15, 16).

## 2. Services and boundaries

Each service below says what it owns, what it enforces, and where the
enforcement runs. "Core" is the Python application layer; "engine" is the
Rust server; "adapter" is the per-CLI half (§6).

### 2.1 Workspace

**Owns.** The directory a session works in. For a worker that is a linked git
worktree cut from the project's landing base, named
`<root>-<wi>` on branch `wi-<n>-<slug>`; for an oversight or ad hoc session
it is still the project root.

**Record (engine).** A `workspace` block on the session record and the
hibernation `Record`:

```
workspace {
  path:           "/home/sprooty/Working/Active/vogt-1298"
  repo_root:      "/home/sprooty/Working/Active/vogt"     # the checkout it was cut from
  branch:         "wi-1298-session-worktree"
  base_ref:       "origin/integrate/vogt-next"
  base_sha:       "99355800…"
  created_at:     RFC 3339
  baseline_dirty: []        # paths dirty at creation; empty for a fresh worktree
  build_dirs:     ["engine/target", "web/node_modules"]   # what `clean` removes
  cleaned_at:     RFC 3339 | null
}
```

`baseline_dirty` is what the git guard (§2.2) protects. For a worktree cut
by the engine it is empty **by construction**, which is what removes the
false positives of WI-1300's second comment: there is nothing to protect, so
nothing is refused.

**Ops.**

- `session.start(worktree=true | {base_ref, branch})` — WI-1298's shape. The
  engine cuts the worktree, records the block, and starts the agent with
  `cwd = path`. A wake returns to the same path. A later session on the same
  work item reuses the worktree when the branch matches.
- `workspace.list(project?, state?)` — every workspace the engine knows,
  with its session, branch, base, size on disk, and whether the branch is
  merged (from the branch collector's `observed` rows). Read scope.
- `workspace.clean(id | path, mode=build_output | remove)` — `build_output`
  deletes `build_dirs` (WI-1304's `worktree_clean.rs`, exposed); `remove`
  runs `git worktree remove` and is refused while a live session has it as
  cwd or while the branch is unmerged and ahead of its base, unless `force`
  is passed by a person. Audited.
- `engine.status.disk` (WI-1304) and a `sweep` row when the volume passes
  `ENGINE_DISK_WARN_PERCENT`.

**Enforced.** Engine: a session may only ask for a worktree of the project
it is bound to; the path must resolve under the workspace root; `remove` is
refused on a live cwd. Core: `session.start` with `purpose=implement` (§2.4)
defaults `worktree=true` for projects whose policy names a landing base.

**Not enforced here.** Reading or building in the shared checkout. The
policy check (§2.2) refuses git *writes* whose repo resolves to another
session's `repo_root`; reads are the agent's business, and once cwd is the
worktree the incentive is gone.

### 2.2 Change and review gate

Four records and two refusals.

**Records.**

- `work.comment kind=verdict` with `VerdictFields {head_sha, base, verdict:
  "no_blockers" | "blockers", blockers: [str], deployment_paths_covered:
  [str], reviewer_session}`. The only place a review result lives. The
  review brief (WI-1299) tells the reviewer to post exactly this. A verdict
  whose `head_sha` is not the branch tip is stale, and `work.land` says so.
- `work.comment kind=verification` with `VerificationFields {head_sha, lane:
  "vogt-dev" | "vogt-prod" | …, what, evidence, by}`. A functional check of
  the change in the deployed setup, posted by a person or by a session that
  is not the worker (the core refuses a verification from the session whose
  hand-off it verifies).
- `handoff` stays as is, with one mandatory free-text line the brief asks
  for: `verified: <what, where>` or `not verified: <why>`.
- The brief composer renders the **latest** hand-off and the **latest**
  verdict, each labelled with date and author, and nothing older (grok
  review fix 9). Superseded verdicts never reach a later brief.

**Landing: `work.land(ref, head_sha, reason)`.** Scope `git-write`. The
caller is a person or a session with the `oversight` role; a worker is
refused. The core:

1. reads the project's `agent_policy.landing` (§2.4): `merge_by` must be
   `oversight`, otherwise the op refuses with "this project's merges are the
   operator's" (msp-agent, msp-klaudia, Versent);
2. requires a `verdict` comment with `verdict=no_blockers` and `head_sha`
   equal to the branch tip the engine reports; a verdict at another SHA is
   named in the refusal;
3. asks the engine for `GitOpReq::Land {repo, branch, head_sha, onto,
   mode}` where `onto` is `landing.base_branch` and `mode` is
   `landing.merge_mode` (`fast_forward` or `rebase`; never `merge`).

The engine: verifies the tip is `head_sha`; rebases the branch onto `onto`
when needed; refuses if the result would contain a merge commit when
`landing.linear_history` is set; runs `forbidden_text` over every commit
message in the range; fast-forwards `onto`; pushes `onto` with the engine's
own forge credential (the same `get` path the broker uses); and returns the
new tip and the commit range. Every step is audited as `event=work.land`
with ref, branch, SHAs and actor, never a diff. A conflict during the rebase
leaves the branch untouched and returns the conflicting paths.

This is the one place merge commits are prevented at the source. The CI
check on `integrate/*` pull requests (WI-1315) stays as the backstop for a
person's own shell.

**Push and PR, by rule not proxy.** `policy.rs:check` gains two rules read
from `SessionPolicy`, which `_enforced_policy` fills from
`agent_policy.landing`:

- *branch ownership*: `git push` is refused unless the ref being pushed
  matches `landing.branch_pattern` with the session's own work item number
  (`^wi-1298-` for the session bound to WI-1298), or the session has the
  `oversight` role, or the caller is a person. `--force` and `--force-with-lease`
  to any branch are refused for workers outright.
- *PR opening*: `gh pr create` is refused when `landing.worker_lands_by` is
  `push_branch` (vogt, codex, rustnzb) and allowed when it is `open_pr`
  (msp-agent, msp-klaudia).

Credentials stay where they are. In `brokered` mode `GH_TOKEN` is a manifest
line; a token for another org (`Versent/*`, brief #14) is a grant
(`session.grant_request kind=credential`), and `vogt-git-askpass` learns one
thing: when the prompt names a host whose token is not in the environment, it
runs `vogt-agent-auth fetch <VAR>` for the VAR the grant named, so an
approved grant is usable by plain `git push` with no agent step. That is a
ten-line change to the askpass and the design's only credential change.

**Commit policy.** Already one place: `session.commit_for` and the hook both
apply `forbidden_text`; `commit_identity` is rendered into the brief. What
moves here is the **discard guard**: `policy.rs` refuses `git checkout --
<path>`, `git restore`, `git reset --hard`, `git clean -f`, `git stash` and
`git rm -f` when a path they reach is in the session workspace's
`baseline_dirty` (or when the paths cannot be read, as Klaudia's gitguard
does). With `baseline_dirty` empty for every engine-cut worktree the guard
costs nothing in the common case and still protects a session opened on a
dirty checkout. msp-klaudia's `gitguard` then defers to the engine when
`VOGT_ENGINE_BROKER_URL` is set (§7).

**Grants.** `session_grant_*` stays the way a session gets a credential or,
in milestone 2, a capability. This design adds no grant kind. "Merge",
"force-push" and "deploy" are not grants but *ops a person or oversight
calls* (`work.land`) or *decisions* (§2.3): an approval that the agent then
performs itself is the shape that broke (brief #15, the classifier refusing
an approved force-push). The decision record gives the approval a durable,
cross-CLI home; the op gives the action an audited, policy-checked executor
that is not the agent's shell.

### 2.3 Attention and decisions

**Attention correctness** is adapter work on the engine's existing contract
(§6): a CLI must make *ready*, *busy*, *awaiting a person* and *turn
boundary* legible. The core pieces that are wrong today and their fixes:

- Klaudia's `RequestHostChange` prompt (WI-1279): the title contract
  `klaudia: awaiting approval` is parsed on the WI-1145 branch and installs a
  synthetic person-only `permission` dialog. Ship it; verify on dev with
  Klaudia **and** the Codex fork template.
- Stale blocked reports (WI-1314 part 2): the engine clears `blocked` when,
  after the report, the session receives input and then starts a new turn,
  or prints `AUTOPILOT: DONE`. A person's `session.answer` that resolves the
  dialog clears it too. `report_unblocked` stays for the agent's own say-so.
- Trail-off (WI-1313, WI-1301): autopilot already nudges any `ready` session;
  the Claude case that sat 13 minutes was either not on autopilot or not
  read as `ready`. That is an investigation first (Opus low), then a fix.
  Klaudia's in-loop nudge (WI-1301) stays a Klaudia change because it acts
  *inside* the turn, which the engine cannot see.

**Decisions.** A new record, deliberately shaped like `SessionGrant` so the
Inbox, push and person-only rule are reused:

```
Decision {
  id:             dec_…
  asker:          actor (a session, usually oversight)
  session:        engine session id of the asker (optional)
  work_item:      WI ref (optional)
  question:       str  ≤ 2000
  options:        [{label, description, recommended: bool}]  2–4, one recommended
  context:        str  ≤ 4000   # the facts the person needs, in the asker's words
  state:          pending | answered | withdrawn | expired
  answer:         {label, note} | null
  answered_by:    actor | null
  created_at, answered_at, expires_at (default 24 h)
}
```

Ops, all audited:

- `decision.ask(question, options, context?, work_item?, reason)` → the
  record, an Inbox entry `decision.pending` with an action card (options as
  buttons, recommendation marked), a push. `work.write` scope. The asker's
  session is recorded from its token (`_own_session`).
- `decision.answer(id, label, note?, reason)` — **a person only**, the same
  refusal as `decide_grant`. Records the answer and, when the asker's session
  is live, types one line into it at its next prompt boundary:
  `[vogt] decision dec_…: <label> — <note>` (the `ci_watch` notice path).
  `session.changes` gains a `decision` change kind so `session.wait_any`
  wakes a waiting overseer.
- `decision.list(state?, work_item?, asker?)` — read scope.
- `decision.withdraw(id, reason)` — the asker or a person.

The asking session does not block: it records the decision, carries on with
whatever does not depend on it, and reads the answer when it lands. An
oversight question that waits eight hours (brief #16) no longer stalls the
sweep. The rule in `.claude/rules/operator-decisions.md` ("ask as a direct
question with options and a recommendation") becomes the record's shape.

A decision is not a grant and not a gate: nothing is applied by the engine
when it is answered. If the answer requires an action (merge, deploy), the
asker performs it through the op that checks it (`work.land`, Komodo via the
operator), and the audit row of that op names the decision.

### 2.4 Dispatch policy as data

Two layers, both already stored and edited through existing ops.

**Per project — `agent_policy` grows three structured blocks** (the free
`rules[]` stay for everything else):

```
landing {
  base_branch:       "integrate/vogt-next"          # workers branch from and land onto
  branch_pattern:    "^wi-{n}-[a-z0-9-]+$"           # {n} is the work item number
  worker_lands_by:   push_branch | open_pr
  merge_by:          oversight | operator
  merge_mode:        fast_forward | rebase
  linear_history:    true
  deploy_by:         operator
}
review {
  template: "claude", model: "opus", effort: "low"
  never_models: ["grok-4.7"]                          # a review on these is refused
  budget_turns: 40
}
verification {
  required_for_done: true                             # agents cannot close without a record
  lanes: ["vogt-dev"]                                 # where "verified" must be exercised
}
```

The 2026-10-13 landing matrix (grok review §3.3) is these fields for five
projects. `_policy_section` renders them as the brief's "Landing rule"
block; `_enforced_policy` ships `landing` to the engine as the push and PR
rules of §2.2.

**Per deployment — a dispatch table in the preference store** (keys
`dispatch.*`, edited with `preference_set`, read by `session.start`):

| Key | Default | Meaning |
| --- | --- | --- |
| `dispatch.profiles` | the C10 table: `implement` = klaudia + grok-4.7, 120 turns, worktree; `implement-careful` = claude + opus, 150; `review` = claude + opus low, 40, read-only; `mechanical` = claude + haiku, 60; `investigate`/`design` = claude + opus low, 60 | `session.start(purpose=…)` resolves template, model, effort, budget, `worktree`, `repo_map`, `autopilot` from the profile; explicit params win and are audited as overrides |
| `dispatch.max_workers_per_overseer` | 5 | live `worker` sessions started by one oversight session |
| `dispatch.escalate_after_failed_reviews` | 2 | after this many `blockers` verdicts on one item, `implement` resolves to `implement-careful` |
| `dispatch.review_by_oversight_only` | true | a `worker` session's `session.start(purpose=review)` is refused |

**Enforced at `session.start` (core).** For an **agent** caller: a model
outside the resolved profile, a review on a `never_models` model, a sixth
worker, or a worker starting a reviewer is refused with the rule named,
unless `override="<reason>"` is passed, which is allowed only for the
`oversight` role, is audited, and raises an Inbox note `dispatch.override`.
For a **person** every parameter is honoured and the mismatch is recorded as
a note, not refused. Nothing here starts a session: the engine still only
runs what a person or an armed trigger asked for (ENGINE.md §8).

**Budgets** are enforced the same way for every CLI because they are
engine-side (`budget.rs`). WI-1281's fix: at the hard turn limit, turn
autopilot off *before* waiting for the prompt (so the re-prompt cannot
outrun the check), and add a cumulative wall clock beside the per-turn one.
The soft line at 80 % is the only budget text the agent sees; briefs carry
no budget prose (fix 11).

### 2.5 Delivery verification

No new workflow states. `work.get` gains a `delivery` projection:

```
delivery {
  landed:    {branch, sha, onto, at} | null     # from work.land or the branch collector seeing the tip on base_branch
  deployed:  [{lane, sha, at}]                   # from deployed.versions: lanes whose receipt is at or past `landed.sha`
  verified:  {lane, head_sha, by, at} | null     # the latest verification comment
}
```

The one gate: `work.transition(to=done)` from an **agent** principal is
refused on a project with `verification.required_for_done` when `verified`
is null, with the message "post a verification comment, or ask a person to
close it". A person is never refused (AGENT_GUIDE §6 keeps compliance a value
to read; this gate is on agents' *claims*, not on the work). The session
economics collector records `verified_by` so the "done vs verified" miss
(brief #13) is countable.

### 2.6 CI watcher

Extensions of `ci_watch.py`, in the same pure-core style:

- **Hung run.** A bound branch's run `in_progress` for longer than
  `ci.hung_after` (default 45 min, per project override) raises Inbox
  `ci.run_hung` with the run URL and the job that is still going. No
  cancel: that is `gh run cancel` from a person or oversight, or an agent
  task a person armed on the `forge-pr-checks` trigger.
- **Every failure in one round.** The vogt engine job gets
  `timeout-minutes`, nextest with `slow-timeout`, and lint/lockfile/test as
  independent steps with `if: always()` (WI-1312). This is a workflow edit,
  not Vogt code, but it is the precondition for the next two.
- **Flaky register.** A label `ci:flaky` on work items whose title names the
  test (WI-1305, WI-1309, WI-1310). `ci_alerts.branch_ci` annotates a failed
  run whose failing job's test names (from the check observation's
  `failed_tests`, when the collector has them) match an open `ci:flaky` item
  as `flaky_suspect` with the item ref, so the Inbox row and the typed notice
  say "probably WI-1309" instead of "red".
- **One bounded rerun, person-armed.** A documented agent task recipe: trigger
  `forge-pr-checks status=failure`, command `gh run rerun --failed <run>`,
  guarded by the task's own run history (one rerun per head SHA). The person
  creates and arms the task; the engine fires it. Nothing in Vogt reruns on
  its own.

## 3. Data model summary

| Record | Layer | New or changed |
| --- | --- | --- |
| `workspace` block on the session record and hibernation `Record` | engine (`pty.rs`, `hibernation.rs`) | new; WI-1298 adds `worktree` on the spawn spec, this adds the record |
| `CodingSession.workspace_path`, `base_sha` | core (`entities.py`) | new fields, nullable |
| `AgentPolicy.landing`, `.review`, `.verification` | core (`entities.py`, `project.update` validation) | new optional blocks |
| `SessionPolicy.branch_pattern`, `.open_pr_allowed`, `.discard_baseline[]`, `.protected_checkouts[]` | engine contract (`engine/contract/src/lib.rs`) | new optional fields |
| `CommentKind` = `note \| handoff \| verdict \| verification`; `VerdictFields`, `VerificationFields` | core (`entities.py`, `models.py`, `work.py`) | new kinds and field sets |
| `Decision` | core (new table, migration), `services/decisions.py` | new entity |
| `dispatch.*` preferences | core preference store | new keys with defaults in code |
| `delivery` projection | core `work.get` | computed, not stored |
| Inbox kinds `decision.pending`, `dispatch.override`, `ci.run_hung`, `workspace.disk` | core `inbox.py` | new rows |
| Audit events `work.land`, `decision.asked\|answered\|withdrawn`, `workspace.clean`, `dispatch.override` | core audit | new |

## 4. MCP tools added or changed

| Op | Scope | Change |
| --- | --- | --- |
| `session.start` | `work.write` | `+worktree` (WI-1298), `+purpose`, `+override`, `+review_of` (WI-1299) |
| `session.input` | `work.write` | `submit` defaults to true; `draft=true` is the opt-out (WI-1316) |
| `session.hibernate` | `work.write` | `superseded_by` accepts either id form (WI-1316) |
| `workspace.list` | `read` | new |
| `workspace.clean` | `work.write` | new |
| `work.comment` | `work.write` | kinds `verdict` and `verification` with their field sets |
| `work.land` | `git-write` | new |
| `work.get` | `read` | `+delivery` |
| `work.transition` | `work.write` | agent-only refusal to `done` when verification is required |
| `decision.ask` / `answer` / `list` / `withdraw` | `work.write` / `read` | new |
| `project.update` | `project.write` | `agent_policy.landing`, `.review`, `.verification` |
| `preference_set` | existing | `dispatch.*` keys |
| `engine.status` | `read` | `+disk` (WI-1304) |

Engine routes added, each with an `ENGINE_COUNTERPARTS` entry:
`GET /api/workspaces` → `workspace.list`; `POST /api/workspaces/clean` →
`workspace.clean`; the `Land` variant on `POST /api/git/op` → `work.land`;
`POST /api/sessions/{id}/notice` (the typed notice `ci_watch` already sends
through `session.input`) stays on `session.input`.

## 5. Enforcement points

Where each rule runs, and what a refusal says.

| Rule | Runs in | Refuses with |
| --- | --- | --- |
| Worktree per worker; cwd = worktree | engine `sessions.rs` spawn | "a worktree needs the checkout it is cut from as cwd" / wrong project |
| Discard guard over `baseline_dirty` | engine `policy.rs` via the hook | "refusing: `git restore` reaches a path that was dirty before this session" |
| Branch ownership on push; no PR where the policy says push-only | engine `policy.rs` via the hook | names the pattern or the landing rule |
| Forbidden text; merge deploys prod | engine `policy.rs` (exists) | names the rule, never the match |
| Land only at a `no_blockers` verdict for the tip; never a merge commit | core `work.land` then engine `GitOpReq::Land` | names the verdict SHA vs tip, or the merge commit |
| Verification from the worker's own session | core `work.comment` | "a verification is posted by someone other than the worker" |
| Agent closes without verification | core `work.transition` | "post a verification comment, or ask a person" |
| Model routing, worker cap, review-by-oversight | core `session.start` | names the profile or cap; `override` for oversight only |
| Budgets | engine `budget.rs` | soft line, then hibernate `trigger=budget` |
| Person-only decisions and grants | core `decisions.py` / `grants.py` | `person_required` |
| Disk | engine `status.disk`, core sweep row | needs_attention, no refusal |

Honest limits, unchanged from ENGINE.md §9 and `policy.rs`'s own header:
sessions share the engine's uid, so a hook, a settings file or a token file
is editable by the agent it constrains. Every rule above narrows *ambient*
behaviour and adds an audit trail; none is a boundary until sessions run as
a separate uid (WI-982). The Claude Code classifier (the driven-session
policy) is a second, independent layer the deployment keeps; a denial there
is a `session.report_blocked`, never a workaround.

## 6. The minimal adapter contract

What a CLI must implement for the engine to drive it. Everything not in this
list is the engine's or the core's, and a CLI that duplicates it is retired
from doing so (§7).

| # | Signal or hook | Claude Code | Klaudia | Codex (fork) | opencode |
| --- | --- | --- | --- | --- | --- |
| A1 | **Launch**: accept `cwd`, model/effort flags, a pinned conversation id and a resume by id, a first prompt naming the brief file | `--session-id`, `--resume`, `--model`, `--effort`, `--settings` (`agent_cli.rs`) | `--session-id`, `--model`, `--prompt-interactive`; no `--effort` | `--session-id` (WI-1144), `-c` overrides | run id pinned; model set |
| A2 | **State in the title**: `<cli>: ready \| working \| awaiting approval \| goal-loop` as the OSC title, or screen shapes `screen.rs` can read | screen heuristics + permission dialog parser | title contract (WI-1090) + `RequestHostChange` (WI-1145) | agent-state title (WI-1145) | idle-box heuristic (`shows_opencode_prompt`) |
| A3 | **Pre-command hook**: post each shell command to `POST /api/agent-auth/policy-check`; exit 2 = do not run | `PreToolUse` → `vogt-policy-hook` | `[[hooks]]` → `vogt-policy-hook` | managed hook in `requirements.toml` — **gap** (WI-1136/1128) | none; pattern deny config only — **gap** |
| A4 | **Accept typed input at the prompt** (nudges, notices, budget line) | yes | yes | yes | yes (per run) |
| A5 | **Report through MCP**: `work.comment handoff`, `session.report_blocked`, `decision.ask` | via Vogt MCP | via `vogt-klaudia-mcp` | via Vogt MCP | via Vogt MCP |
| A6 | **Conversation link** (only when a person types the CLI into a shell) | `SessionStart`/`SessionEnd` hook | not needed (engine pins) | not needed | not needed |
| A7 | **Tools resolve paths against cwd** and say so; Grep returns content by default | yes | Grep defaults to files only (WI-1311) — **fix** | n/a | n/a |

A2 is the whole of attention correctness: the engine's `oversight.classify`
reads only what A2 yields. A3 is the whole of per-command enforcement. A5 is
CLI-agnostic by construction. What each CLI **stops** doing once the engine
rule is live: Klaudia's `gitguard` baseline (engine discard guard, §2.2);
per-brief process prose (policy blocks); per-CLI budget prose (engine soft
line).

## 7. Migration from today's per-CLI guards and hooks

| Today | After | When |
| --- | --- | --- |
| Klaudia `gitguard` captures a dirty baseline at process start, in the session cwd (the main checkout), and refuses by relative path (WI-1300, false positives on 2026-10-13) | Engine captures `baseline_dirty` per workspace at creation; `policy.rs` enforces the discard rules; Klaudia's `gitguard` is **off when `VOGT_ENGINE_BROKER_URL` is set** and on otherwise (a Klaudia outside Vogt keeps its guard) | phase 3, after the engine discard guard is live and reviewed |
| Claude classifier refuses approved force-pushes, flags branch pushes as deploys (brief #15) | Unchanged classifier; the approval becomes a `Decision`, the action a `work.land` or a person's own shell. No agent force-push exists to approve. | phase 1–2 |
| Oversight merges in its shell; merge commits reach `integrate/*` (WI-1315) | `work.land` fast-forwards or rebases; CI check backstops a person's shell | phase 2 |
| Briefs retype rules, budgets and landing per session | `agent_policy.landing` + `dispatch.profiles` render once; budgets only from the engine | phase 1 |
| Hand-off claims read as results (WI-1299) | Review brief labels the hand-off as a claim; `verdict` record is the only review output | phase 0–1 |
| Blocked flags cleared by hand (WI-1314) | Engine clears on the next turn after input, on `AUTOPILOT: DONE`, or on a person's answer | phase 3 |
| `session_input` without Enter; `superseded_by` wants the engine UUID; no-item workers cannot hand off (WI-1316) | `submit` default true; either id everywhere; `session.start(purpose=implement)` without `work_item` creates a `local_only` chore named after the session | phase 0 |
| Operator decisions as lines in status updates; sweep blocked for hours | `decision.ask` → Inbox card on the phone → typed answer | phase 1 |
| Codex and opencode run with no policy hook | Codex managed hook once the fork image is the pod (WI-1142); opencode keeps the pattern deny and is documented as a gap | later; not in this initiative |

Nothing is removed before its replacement is verified on dev. Each phase
below is independently shippable.

## 8. Threat model: what an agent can no longer do

With this design live, a driven agent session (any CLI, default posture):

1. cannot start in, or `git`-write to, the shared main checkout: its cwd is
   its worktree and the policy check refuses writes whose repo resolves to a
   `protected_checkouts` path;
2. cannot discard changes that pre-date its session (`baseline_dirty`), nor
   be refused for discarding its own (the baseline is empty for a fresh
   worktree);
3. cannot push to `main_branches`, to another item's branch, or with
   `--force`; cannot open a PR where the project lands by branch push;
4. cannot land its own branch (`work.land` is oversight or a person), and
   oversight cannot land without a `no_blockers` verdict at the exact tip,
   nor leave a merge commit on `integrate/*`;
5. cannot post the verification of its own work, nor close an item that
   needs one;
6. cannot run past its turn or wall-clock budget, or be nudged forever;
7. cannot start a reviewer, start a sixth worker, or start a worker on a
   model the profile forbids without an audited `override` that only
   oversight may pass;
8. cannot answer a decision or a grant, or clear another session's block
   through `session.answer` on a person-only dialog;
9. holds no credential beyond the manifest and its live grants in `brokered`
   mode. In `identity` mode (this deployment today) it holds the machine
   identity, so 9 does not hold here until the operator moves to `brokered`
   (oversight-grants.md open question 6).

What it still can do, by design: read anywhere it is given, build and test
in its worktree, commit on its branch, push its branch, post hand-offs,
blocked reports and decisions, ask for grants. What remains unenforced at
the kernel: everything in §5's honest limits.

## 9. Rollout in phases, with the work items

Each phase leaves every existing flow working. Items are sized for one
session; the model tag says who runs it. **Opus low** marks investigation,
design or security-sensitive work where grok or Haiku is not appropriate.
Every H-item below names its Vogt ref; the initiative is `agent-harness-services` (ini_01M4SKSTT8WGPB3XQHTT8A4WAH). Labels `model:haiku`, `model:grok`, `model:opus-low` carry the tag; `ci:flaky` marks the flaky register.

**Phase 0 — land what is cut (this week).**
Finish and land the in-progress grok branches: WI-1298 worktree start,
WI-1304 build-output cleanup and disk status, WI-1281 budget enforcement,
WI-1299 review brief (its six blockers). Fix the MCP footguns (WI-1316 split
into three: WI-1316 submit default, WI-1337 either id form, WI-1338 a local
item for unbound autopilot workers), the CI job timeouts and `if: always()` (WI-1312), and document
the landing flows (WI-1302). Ship WI-1279's host-change detection and verify
it on dev.

**Phase 1 — records and policy as data.**
`agent_policy.landing/review/verification` (H1 (WI-1318)); `verdict` and
`verification` comment kinds (H2 (WI-1319)); brief composer renders the latest
hand-off and verdict only (H3 (WI-1320)); `Decision` entity and ops (H4 (WI-1321), Opus low:
first slice of a new subsystem); Inbox card and typed answer (H5 (WI-1322)); dispatch
profiles and `purpose` on `session.start` (H6 (WI-1323)).

**Phase 2 — gates.**
Workspace record and `workspace.list` (H7 (WI-1324)); `workspace.clean` and
merged-branch removal (H8 (WI-1325)); engine discard guard (H9 (WI-1326), Opus low); branch
ownership and PR rules in `policy.rs` (H10 (WI-1327), Opus low); `work.land` (H11 (WI-1328),
Opus low); linear-history CI check (WI-1315, re-scoped); dispatch
enforcement at `session.start` (H12 (WI-1329)); `delivery` projection and the
agent-only done gate (H13 (WI-1330)); askpass fetches a granted token (H14 (WI-1331)).

**Phase 3 — adapters converge.**
Klaudia `gitguard` defers to the engine (H15 (WI-1332), Opus low, msp-klaudia); Klaudia
Grep content default (WI-1311, re-scoped); blocked auto-clear (WI-1314,
re-scoped); autopilot trail-off investigation (WI-1313, Opus low); Klaudia
in-loop trail-off nudge (WI-1301); adapter contract documented in ENGINE.md
(H16 (WI-1333)).

**Phase 4 — CI watcher.**
Hung-run detection (H17 (WI-1334)); flaky register (H18 (WI-1335)); person-armed rerun recipe
(H19 (WI-1336)). Flaky items WI-1305, WI-1309, WI-1310 are labelled `ci:flaky`.

Dependencies: H2 (WI-1319) → H3 (WI-1320), H11 (WI-1328), H13 (WI-1330); H1 (WI-1318) → H10 (WI-1327), H11 (WI-1328), H12 (WI-1329), H13 (WI-1330); H4 (WI-1321) → H5 (WI-1322); H6 (WI-1323) →
H12 (WI-1329); WI-1298 → H7 (WI-1324) → H8 (WI-1325), H9 (WI-1326), H15 (WI-1332); WI-1304 → H8 (WI-1325); H9 (WI-1326) → H15 (WI-1332); WI-1312 → H17 (WI-1334), H18 (WI-1335).

## 10. Operator decisions (answered 2026-10-13)

The operator answered every question below on 2026-10-13, each with the
recommended option, plus one sequencing decision: **the design is reviewed
(Opus low) before any item starts**, including the phase-0 and phase-1
grok/Haiku items. Decisions, in order:

1. `work.land` callers: oversight-role sessions and persons.
2. Agent-only done gate: on for vogt, msp-agent and rustnzb; off for codex
   and msp-klaudia until their deploy lanes are collected.
3. `override` on `session.start`: kept for the `oversight` role, audited,
   with an Inbox row.
4. Klaudia's gitguard turns itself off inside Vogt sessions once the engine
   discard guard (WI-1326) is live on dev.
5. Agent-auth mode: vogt-dev moves to `brokered` once WI-1331 is verified;
   prod a week later.
6. Decisions expire at 24 h with no implied answer; the asker decides what
   expiry means. Maximum TTL 7 days.
7. `dispatch.max_workers_per_overseer` defaults to 5.

The questions as asked, kept for the record:


1. **Who may call `work.land`.** Oversight sessions and persons (this
   design), or persons only, with oversight asking a decision? Recommended:
   oversight, since the verdict-at-tip and linear-history checks are the
   gate, and a person still approves the integration PR to `main`.
2. **Agent-only done gate.** Accept that an agent cannot close an item on a
   project with `verification.required_for_done` while a person can?
   Recommended: yes for vogt, msp-agent and rustnzb; off for codex and
   msp-klaudia until their deploy lanes are collected.
3. **Dispatch override.** Should `override` exist at all for agents, or
   should a routing mismatch always be a `decision.ask`? Recommended: keep it
   for the `oversight` role, audited and surfaced in the Inbox.
4. **Discard guard ownership.** Turn Klaudia's `gitguard` off inside Vogt
   sessions once the engine guard is live (phase 3), or run both? Recommended:
   off inside Vogt; two guards with two baselines is how the false positives
   happened.
5. **`identity` mode.** Threat-model item 9 does not hold in this
   deployment. Move the dev stack to `brokered` once grants and the askpass
   fetch (H14 (WI-1331)) are verified? Recommended: dev first, prod after a week.
6. **Decision expiry.** 24 h default, then `expired` and the asker is told to
   proceed on its recommendation or stop? Recommended: expire with no
   implied answer; the asker decides what expiry means.
7. **Worker cap.** Five workers per oversight session as the default
   (`dispatch.max_workers_per_overseer`)?

## 11. What this design does not do

- No new permission classifier and no change to the driven-session policy
  files. Denials stay reports.
- No autonomous rerun, merge, deploy or session start (ENGINE.md §8).
- No per-CLI reimplementation of any rule in §5; a CLI without A3 is a
  documented gap, not a second enforcement.
- No workflow-state changes; `verified` is a record and a projection.
- No credential proxy for `git push`; credentials stay manifest lines and
  grants, with one askpass change.
- No Vale or other prose linting (WI-1297 §1).
