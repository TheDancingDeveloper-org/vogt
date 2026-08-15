# Vogt × MyDevEnv2 — Single-Product Merge Investigation

Status: **investigation** · 2026-08-14 · source branch: `MyDevEnv2@dev`
Decisions taken as input: one repo / one product; GUI converges on the
Solid/Vite PWA stack; backend stack is an open question this document
answers; deliverable is this design + phased plan (no code yet).

---

## 1. Executive summary

Merge MyDevEnv2 into the Vogt repository as Vogt's **session engine**, keep
both backends in their current languages (Python vogt core, Rust session
server), put the **Rust server in front** as the single port / single
container entrypoint, and rebuild Vogt's GUI as new surfaces inside the
existing Solid PWA. The mobile MVP1 is the existing Capacitor shell pointed
at the merged PWA.

Why this shape:

- The two products are **complementary, not overlapping**: vogt owns the
  *work* (projects, backlog, bugs, drift, audit, contract), MyDevEnv2 owns
  the *doing* (PTYs, sessions, agent tasks, assistant, files, git UI,
  push, voice). There is almost no duplicated domain logic to reconcile.
- Integration has **already started from the MyDevEnv2 side**: `dev`
  registers Vogt's MCP server for agents running inside its sessions
  (`1dd8e14`), and both stacks already deploy to Node B via Komodo from
  `indexarr/ops`. The merge formalizes a coupling that exists.
- A backend rewrite in either direction is the single largest cost on the
  table (§4) and buys nothing user-visible. Two processes in one container
  behind one port is a well-understood shape and is exactly what vogt's own
  deployment doc calls a door left open ("transports are thin adapters").

One deliberate design reversal must be recorded: vogt's `DESIGN.md` §1.2
lists *"Being an agent runner"* as a v1 non-goal. This merge overturns
that. The merged product both *tells agents what and why* and *runs them*.
That is a requirements change, not drift, and should be written into
`REQUIREMENTS.md` as such when implementation starts.

---

## 2. Current-state inventory

### 2.1 Vogt (`main`, v1 complete M0–M7)

| Aspect | State |
|---|---|
| Language / size | Python (uv), ~16.5k lines across 85 files |
| Architecture | Operation registry (58 operations) → exposed identically over CLI, REST (FastAPI), MCP (streamable HTTP + stdio) |
| Storage | SQLite |
| Domain | Projects, work items (backlog/bugs/ranking), workflows, labels, initiatives, actors, collectors (git-local, source markers, dep-refs, GitHub issues/PRs/Actions/releases), contract checks, drift proposals, suppression, audit, GitHub write-back, `project import` (clone + register + consolidate), notifications inbox |
| GUI | Vanilla JS, ~1k lines total (`app.js` + 26-line HTML + CSS), served at `/ui`, consumes only public REST |
| Auth | Scoped bearer tokens bound to actors, double-gated writes, every write audited with who/what/why |
| Deployment | One process, one port (`/ui`, `/api`, `/mcp`, health), in-process TLS, Docker on Node B via Komodo from `indexarr/ops`, GHCR image, tag-triggered signing |

### 2.2 MyDevEnv2 (`dev`)

| Aspect | State |
|---|---|
| Language / size | Rust/Axum server ~9.8k lines; Solid/Vite/TS PWA (Terminal, Editor+Monaco, FileTree, Git, History, Assistant, AgentTasks, Continuity, Gui-launch, CommandPalette, Settings); Capacitor 8 Android shell |
| Sessions | Per-session PTY, ring-buffer scrollback, WS attach with snapshot replay, activity state machine (idle/running/waiting-for-input/errored), SSE event stream, session templates (command/cwd/env matched by repo name / path prefix) |
| Agent tasks | Durable scheduled-agent registry (`manual` / `interval` / UTC `daily`), runs launch real PTY sessions through `SessionRegistry`, prompt + persistent context files, `MYDEVENV2_NOTIFY:` push hook. Command is user-supplied (codex/claude/anything) |
| Assistant | Server-side OpenAI-compatible tool-use loop; tools: `list_sessions`, `read_session_tail`, `send_input` (confirmation-gated PendingAction, 120 s expiry, one at a time); PWA Assistant tab; STT on-device (APK push-to-talk), TTS via `speechSynthesis`. Off unless API key set. Threat model documented (`docs/ASSISTANT.md`) |
| Other API | Files (read/write/op/download/tree/search), git (status/diff/log/branch/op), GUI process launch/stream, push (VAPID + FCM), history (persisted session logs + search index), ContextKeeper continuity proxy |
| Auth | Single primary token + extra scoped tokens with 8 capabilities (`sessions`, `filesystem-write`, `git-write`, `gui-control`, `agent-tasks-write`, `push-write`, `history-write`, `assistant`), per-token mutation rate limit |
| Deployment | Docker on Node B via Komodo from `indexarr/ops` (`prod-mydevenv2` at 8910; `dev-mydevenv2` at 8911 per `uplift.md`), Woodpecker CI, Tailscale in-container, Caddy fronting, `main`→prod / `dev`→dev image streams |
| Mobile | Capacitor shell loads the deployed PWA; APK built in CI, published to Forgejo `apk-latest`; STT plugin + `RECORD_AUDIO`; push via FCM |

### 2.3 Overlap analysis

Genuine overlap is small and all infrastructural:

- **Two HTTP servers** (FastAPI vs Axum) — resolved by fronting (§5).
- **Two token models** — resolved by capability mapping (§9).
- **Two GitHub relationships** — vogt's forge adapter vs sessions running
  `gh`/agents; no conflict, different layers.
- **Two GUIs** — resolved by convergence on Solid (decided).
- **Workspace layout** — vogt's import root vs MyDevEnv2's
  `workspace_root`; must be unified so a session can open in an imported
  project (§6.3). This is the one *semantic* join point.

---

## 3. Product shape

**Vogt is the product; MyDevEnv2 becomes its session engine.** The merged
product is Jira-shaped project governance where every work item and every
project can *open a coding session*: a PTY running the user's chosen agent
(or a plain shell) in that project's working tree, with the work item's
brief injected, watchable and steerable from browser or phone, with the
assistant able to talk about both the *work* (vogt domain) and the
*terminals* (session domain), by voice.

Naming: this document assumes the product keeps the name **Vogt** and the
session engine keeps its identity internally (crate names, config prefix)
until a deliberate rename. An open question (§11) covers what happens to
`mydevenv2.sprooty.com` and the `MYDEVENV2_*` env surface.

---

## 4. Backend stack decision

Three options were weighed:

| Option | Cost | Risk | Verdict |
|---|---|---|---|
| **A. Keep both** (Python core + Rust engine, one repo, one container, one port) | Low — glue only | Low; both codebases stay tested as-is | **Recommended** |
| B. Converge on Rust | Port ~16.5k lines of domain logic, 58 operations, SQLite schema, collectors, GitHub adapter, MCP server, audit semantics. Multi-month, high regression surface (vogt's acceptance-test suite would need porting too) | High | Defer; revisit only if the two-process shape proves painful in operation |
| C. Converge on Python | Port PTY/WS/scrollback/activity engine, push, assistant loop; lose rust-embed PWA serving and the performance headroom of the session engine; Python PTY fan-out under many attached agents is the weak spot | High | Rejected |

**Recommendation: A.** The interface between the two halves is naturally
narrow — vogt needs perhaps four things from the engine (create session,
list sessions, session status events, kill), and the engine needs perhaps
three from vogt (project registry lookup, work-item brief, MCP
registration). Narrow interfaces are what multi-process shapes are for.
Rust convergence (B) remains the long-term escape hatch and nothing in
option A forecloses it: the internal HTTP interface between front door and
vogt core is exactly the seam a future port would replace.

---

## 5. Repository and process architecture

### 5.1 Repo merge mechanics

Merge MyDevEnv2 into the vogt repo **with history**, using
`git subtree add` (or `git merge --allow-unrelated-histories` after a
prefix rewrite) from `MyDevEnv2@dev`:

```
vogt/
├── src/vogt/            # Python core (unchanged layout)
├── engine/              # ← MyDevEnv2 server/ + contract/ (Rust workspace)
│   ├── server/
│   └── contract/
├── web/                 # ← MyDevEnv2 web/ (Solid PWA, becomes THE GUI)
├── mobile/              # ← MyDevEnv2 mobile/ (Capacitor shell)
├── docs/                # merged; MyDevEnv2 docs land under docs/engine/
├── deploy/              # one compose stack (§10)
└── ...
```

Notes:

- `MyDevEnv2/client/` (archived GPUI desktop client) is **not carried
  over** — it is dead reference code; the MyDevEnv2 repo remains as the
  archive.
- `src/vogt/gui/static/` (the ~1k-line vanilla GUI) is retired once the
  Solid surfaces reach parity (§7); until then it keeps serving at
  `/ui-legacy` behind the front door so nothing regresses mid-transition.
- CI merges: Woodpecker pipelines (Rust fmt/clippy/test, web typecheck,
  APK) join vogt's existing test/publish pipeline; the `dev`-branch /
  `:dev` image-stream discipline from `uplift.md` carries over to the
  merged repo and should be adopted repo-wide (it is a prerequisite the
  MyDevEnv2 backlog already names).

### 5.2 Process model — Rust front door

One container, one published port, two processes:

```
supervisor (container entrypoint)
├── vogt-engine (Rust/Axum)  ← the single listening port
│   ├── /                      Solid PWA (rust-embed, incl. all vogt surfaces)
│   ├── /api/sessions|files|git|assistant|agent-tasks|push|history|...  (native)
│   ├── /api/vogt/...          reverse-proxy → 127.0.0.1:<internal>
│   ├── /mcp                   reverse-proxy → vogt core MCP (streamable HTTP)
│   ├── /version /health/live /health/ready   reverse-proxy → core (§5.3)
│   ├── /connection-info       synthesised by the door, not proxied (§5.3)
│   ├── /api/sessions/:id/attach   WebSocket (native)
│   └── /healthz /readyz       aggregate: engine + probe of vogt core
└── vogt-core (Python/uvicorn) ← loopback only, never published
    ├── /api/... (58 operations)  ├── /mcp  └── /health/*
```

Why the Rust side fronts rather than the Python side or an extra Caddy:

- It already embeds and serves the PWA (`rust-embed`) and already speaks
  WebSockets; proxying WS through FastAPI adds a fragile hop on the
  hottest path (terminal I/O).
- Axum reverse-proxying a loopback HTTP service is a small, boring amount
  of code; the streamable-HTTP MCP transport proxies as plain HTTP.
- Vogt's own deployment invariant — *any port that serves MCP also serves
  plain HTTP health* — is preserved at the front door.
- A supervisor (`s6-overlay`, `tini` + small script, or compose
  `depends_on` with two services in one stack) keeps "one product"
  honest: one image or one stack, one healthcheck, one deploy act.
  Two containers in one compose stack is an acceptable fallback if
  in-container supervision fights the existing Tailscale sidecar setup.

### 5.3 The front door's identity (r10)

§5.2 lists what the door proxies. It did not say what the door *is*, and the
gap had a cost: the first real import found the door answering Vogt's
documented probes with the PWA at 200, and `connect` handing clients an MCP
URL pointing at the core's internal address. This section settles it.
FR-A7, FR-A8, FR-A9 and NFR-Q7 are the requirements it governs.

**The rule.** *A client-facing fact about an address belongs to the process
that publishes that address.* Everything else follows.

**Who knows the public URL.** The door, and only the door. r7 established
why a server cannot infer this: it binds a container port and is published
somewhere else, so the URL is an *exposure* value under NFR-D2 — configured,
no default, reported absent rather than guessed. That argument does not
weaken one hop out, it repeats. The core binds loopback and is published
by the engine at an address it has no way to learn. So:

- The **engine** gains its own public-URL configuration, with no default,
  and reports it absent when unset.
- The **core** keeps `public_url` for the core-only shape, where the core
  *is* the door. In the merged shape the core's value is an internal detail
  and no client ever sees it.

That last point is a property worth having: it makes the current
misconfiguration — `VOGT_PUBLIC_URL=http://vogt-dev.tailc7d3c.ts.net:8910`
on a stack reached at `https://vogt-dev.sprooty.com` — stop mattering,
rather than requiring an operator to keep two addresses in sync forever. A
design that removes a class of misconfiguration beats one that documents it.

**Which paths the door serves, and how.** Split by whether the answer says
anything about addressing:

| Path | Treatment | Why |
|---|---|---|
| `/version` | proxy verbatim | name and version only; true at every hop |
| `/health/live`, `/health/ready` | proxy verbatim | the core's own liveness and applied schema; the engine's aggregate stays at `/healthz`, `/readyz` |
| `/connection-info` | **synthesised by the door** | every field is addressing |
| `/api/vogt/connect` | **rendered by the door** | it emits a client configuration, which is addressing in its most consequential form |

Synthesised, not rewritten: the door composes the answer from what it knows
(`url` = its own public URL, `api_path` = `/api/vogt`, `mcp_path` = `/mcp`,
`health_path` = `/health/ready`) plus what only the core can say (`version`,
`supported_mcp_protocol_versions`, `writes_enabled`). Version skew therefore
stays truthfully reported rather than being flattened by the proxy.

**`api_path` at each hop.** `/api` at the core, `/api/vogt` at the door.
Both are correct where they are stated, which is exactly why the door must
not repeat the core's answer: a client that reads `/api` from the door's
`/connection-info` will call a path the door does not serve.

**What a client discovers, and from where.** One address, asked once. A
client — including `vogt-mcp-remote` — reads `/connection-info` from the
address a human gave it, and everything it needs is in that answer. It never
learns an inner address, and it is never expected to know which shape it is
talking to: the core-only and merged deployments answer the same four probes
with the same field names, and differ only in the values that are genuinely
different.

**Failure shapes.** A probe path is served or refused with a named reason
(the `503` with the refusing-half header the proxy already emits). It is
never handed to the SPA fallback — FR-A7's clause exists because a `200`
carrying `index.html` is the one failure a client cannot detect. And per
FR-A6, none of this can block a client: a door that cannot reach its core
still forwards `/mcp`, and a discovery answer that cannot be parsed is a
warning, not an exit.

---

## 6. Coding sessions from vogt (the core new capability)

### 6.1 User-visible behavior

- On a **work item**: "Start session" → PTY opens in the owning project's
  working tree, running a chosen **session template** (agent CLI or
  shell), with a generated prompt file containing the work item's brief
  (`why`, description, relations, acceptance notes) — reusing the
  agent-tasks prompt-file mechanism verbatim.
- On a **project**: "Open terminal" → plain shell in the project tree.
- Sessions started from work items are **linked**: the session id is
  recorded on the work item (audited, with why = "session started for
  WI-123"), the work item view shows live activity state
  (idle/running/waiting-for-input/errored) via the existing SSE stream,
  and the terminal tab deep-links back to the work item.
- Agents inside the session get **Vogt MCP registered automatically** —
  MyDevEnv2 `dev` already does this (`vogt-mcp-auth.sh`, `1dd8e14`); the
  merge makes it first-party: the session's MCP registration carries an
  actor-scoped token so agent writes to vogt are attributed to that
  session's actor.
- **Agent tasks** (scheduled) gain an optional `work_item` /  `project`
  binding, so recurring checks can file their findings into vogt as
  observations instead of only pushing notifications.

### 6.2 Plumbing

Direction of calls: **vogt core → engine** over loopback HTTP with a
service token holding only `sessions` capability. New vogt operations
(joining the 58 in the registry, hence automatically on CLI/REST/MCP):

- `session.start(work_item | project, template?, prompt_overrides?)`
- `session.list(project?)` — enriched with vogt linkage
- `session.stop(id)`
- (read-only) session activity is consumed via the engine's SSE stream by
  a small collector, so session outcomes become vogt observations with
  freshness/trust like everything else — exit code, duration, resulting
  git delta (the engine's git API can report the dirty state the session
  left behind).

### 6.3 Workspace unification (the one hard join)

Vogt's **import root** (where `project import` clones) and the engine's
**`workspace_root`** must be the same tree, and vogt's project registry
becomes the authority the engine's session templates consult
(`match_repo_names` / `match_path_prefixes` today; a
project-registry lookup tomorrow). Rule: *a session opened "for" a vogt
project always opens in the path the registry records for it.* Projects
registered from pre-existing local paths (not imported) work identically —
the registry stores the path either way.

---

## 7. GUI uplift — Jira/Trello-grade on the Solid PWA

The Solid PWA becomes the single front-end. Existing tabs (Terminal,
Editor, Files, Git, History, Assistant, Tasks, Continuity) remain; vogt
surfaces are added as new top-level routes sharing the shell, command
palette, settings, and push plumbing.

New surfaces, in build order:

1. **Board** — Kanban of work items, columns = workflow states (workflows
   are data in vogt, so columns come from `workflow.list`, not
   hard-coding). Drag-drop = `work.transition` (which enforces legal
   transitions server-side — illegal drops bounce with the server's
   reason). Swimlanes by project or initiative; filters by label, type,
   project, actor; WIP counts.
2. **Work item detail** — modal/panel: description, state history,
   comments (`work.comment`), relations graph, labels, audit trail for
   this item, collected evidence (freshness/trust badges as the current
   GUI shows), and the **Start session** button (§6).
3. **Backlog & bugs** — ranked global tables (`backlog`, `bugs`, `why`
   for the explainable ranking), inline quick-create, bulk label/transition.
4. **Project pages** — per-repo state, CI, contract/compliance panel,
   drift inbox (accept/reject), dependency graph, import/onboarding flow.
5. **Global** — audit browser, notifications inbox, actor/token admin.

Vogt's existing GUI invariant carries over and gets stronger: the PWA
consumes **only the public REST API**, and the existing "resolve every URL
in the shipped JS against the operation registry" acceptance test extends
to the Solid bundle (resolve against *both* the vogt registry and the
engine's `API_CONTRACT.md`).

Trello/Jira "detail" items worth scoping honestly: drag-drop with
optimistic update + server-authoritative rollback; keyboard-driven triage
(the CommandPalette already exists — extend it with vogt verbs);
saved filters; and *nothing else Jira-shaped* — vogt's non-goals (sprint
ceremonies, burndown, time tracking) stand unless separately revisited.

## 8. AI layer — the assistant learns the vogt domain

The engine's assistant is the right foundation: server-side tool loop,
confirmation-gated effectors, documented threat model, voice already wired
(STT in APK, TTS everywhere). Changes:

1. **Tool surface grows** from 3 tools to 3 + a curated slice of vogt's
   operation registry — read-mostly first (`backlog`, `bugs`, `why`,
   `project.brief`, `work.list`, `work.get`, `compliance`, `drift` reads),
   exposed to the loop by generating tool schemas from the registry
   (the registry is typed; this is mechanical).
2. **Writes stay gated.** Vogt-mutating tools (`work.create`,
   `work.transition`, `work.comment`, `session.start`) reuse the
   `PendingAction` approve/deny card exactly as `send_input` does today.
   One pending action at a time, 120 s expiry, on-screen tap — the voice
   path never auto-approves. Vogt's audit `why` field is populated from
   the conversational context ("assistant, approved by user").
3. **Conversation grounding**: "what should I work on?" → ranked backlog
   with vogt's explainable `why`; "start a session on the top bug" →
   gated `session.start`; "what did the agent in session 3 just do?" →
   `read_session_tail`. This is the product's signature interaction.
4. **Backend note**: the assistant is OpenAI-compatible-only today, and
   validation notes say the `claude-*` proxy routes hung. Making the loop
   provider-clean (or adding a native Anthropic path) is in scope for
   this phase, since the merged product will lean on the assistant far
   harder. Voice itself is explicitly *not yet proven* ("hasn't been put
   through its paces") — stage M11 includes a validation pass, not just
   feature work.
5. **Threat model extension**: vogt data (issue titles, imported GitHub
   content) is *external content* by the assistant's own rule and gets
   the same untrusted-data wrapping as terminal output. Tool results from
   vogt reads must be delimited like `<terminal-output>` is today.

## 9. Auth unification

Two models today: engine capabilities (8-variant enum, single primary +
extra scoped tokens) vs vogt actor-bound scoped tokens with double-gated
writes and audit attribution. Unification plan:

- **One token namespace at the front door** (the engine's), extended with
  vogt capabilities (`vogt-read`, `vogt-write`, `vogt-admin` — final
  granularity to be set against vogt's existing scope list).
- The front door **maps** its token → a vogt actor: each front-door token
  is provisioned with a paired vogt token bound to a named actor; the
  proxy injects it. Vogt's audit therefore records real actors, not
  "the proxy". Session-spawned agents get per-session actor tokens (§6.1).
- Vogt's double-gated writes and per-write `why` are **not weakened** —
  the proxy forwards, it does not pre-approve.
- Per-token mutation rate limiting stays at the front door.

## 10. Deployment

Both stacks already live on Node B, deployed by Komodo from
`indexarr/ops`, fronted by Caddy, tailnet-addressed. Merged shape:

- One stack (`dev-vogt` first, then `prod-vogt`) replacing the
  four existing stacks over time (`prod/dev-mydevenv2`, `personal/vogt`).
- One image (or one two-service compose, per §5.2), one published port;
  Tailscale sidecar pattern and the ContextKeeper `extra_hosts` pinning
  carry over unchanged (Tailscale breaking container DNS is already a
  known landmine — keep the pin).
- The `uplift.md` dev/prod environment split (dev branch → `:dev` images →
  dev stack → validate → fast-forward main) becomes the merged repo's
  standard flow. Several merge phases (mobile, voice, push) are only
  verifiable against a live dev stack — this split is an M8 prerequisite
  (NFR-D12), exactly as MyDevEnv2's backlog already concluded.
- State: vogt SQLite volume + engine `state_dir` + shared workspace volume
  (§6.3) in one stack; backup/restore story must cover all three (vogt
  already has backup/restore; extend the act to include engine state).

## 11. Open questions

1. **Name & domain** — does `mydevenv2.sprooty.com` become
   `vogt.sprooty.com`? Config env prefix consolidation (`VOGT_*` vs
   `MYDEVENV2_*`) — recommend new `VOGT_*` names with the old ones
   accepted as aliases for one transition period.
   **Resolved at M14 (2026-08-14): the merged product is served at
   `vogt.sprooty.com`, and the product is Vogt.** `mydevenv2.sprooty.com`
   remains the standalone engine's own host until that stack is retired, and
   both origins are in the merged CORS list for the transition — an origin
   removed early is a CORS failure in a browser somebody is using, and one
   extra entry costs nothing.

   The **config prefixes are a separate decision and are deliberately not
   renamed yet.** As built they divide by *process*, not by product: the
   engine reads `MYDEVENV2_*` for what it owns and `VOGT_CORE_*` for the core
   it fronts, and vogt-core reads `VOGT_*`. Renaming them is a stack-env
   migration on a live deployment, and doing it in the same change as the
   host move would mean two ways for one cutover to fail. The sunset order
   is: move the host, retire the standalone stack, *then* alias the
   `MYDEVENV2_*` names and sunset them. Until then the compose comments carry
   the explanation, which is the cost of the delay and is written down as
   such.
2. **MyDevEnv2's standalone life** — does anything keep needing MyDevEnv2
   *without* vogt? If yes, the engine must stay bootable with vogt-core
   absent (it degrades naturally today; recommend preserving that
   property — it also keeps the engine testable alone).
   **Resolved at M9**: preserved and asserted rather than assumed. FR-E9 is
   the requirement; `engine/server/tests/vogt_core.rs` boots the engine with
   no core configured and with a core that will not answer, and checks that
   sessions keep working, that readiness stays green (the core's probe is
   reported and deliberately not fatal), and that the Vogt routes refuse
   with a named reason instead of an empty answer.
3. **ContextKeeper and cadastre** — both sidecars integrate with
   MyDevEnv2 `dev` today. Assumed carried over unchanged; cadastre's
   overlap with vogt (both "estate registers") deserves its own look
   later, not in this merge.
4. **Assistant provider** — resolve the hung `claude-*` proxy routes or
   add a native Anthropic client before the assistant becomes the
   headline interaction (§8.4). Carried into r9 as **FR-T7**, priority C:
   the merge does not settle it, and M12 attempts it rather than promising
   it.
5. **Vogt REQUIREMENTS.md amendment** — §12 drafts this as revision r9
   in the document's own format (numbered FR/NFR, append-only IDs); it
   must be folded into `REQUIREMENTS.md` proper before M9 starts, per
   vogt's own working style. **Resolved 2026-08-14**: folded as revision r9
   of [`REQUIREMENTS.md`](REQUIREMENTS.md), with §13 into its §3, §14 into
   [`ROADMAP.md`](ROADMAP.md) as M9–M14, and the `DESIGN.md` §1.2 reversal
   recorded there in place.
6. **Editor/Files/Git tabs vs vogt's forge view** — the PWA's git tab
   operates on working trees; vogt's project pages show forge state. They
   should cross-link (project page → open git tab in that tree), not
   merge. **Answered in the negative for M11, deliberately**: the two stayed
   separate and the cross-link was not built. A project page knows the
   registry's path for a project, so the link is available to build; what
   stopped it is that nothing has been rendered in a browser yet, and a
   navigation between two surfaces is exactly the kind of claim that should
   not be made untested.

## 12. Proposed requirements baseline (draft revision r9)

> **Folded — no longer authoritative (2026-08-14).** This section and §13 are
> now revision **r9** of [`REQUIREMENTS.md`](REQUIREMENTS.md): the revision
> note and the appended IDs live in its §1/§2, and §13's deferrals in its §3.
> `REQUIREMENTS.md` governs; the draft below is kept as the working record of
> where r9 came from. Two things changed in the fold, both recorded in the r9
> revision note: the NFR-S family was appended to without appearing in the
> draft's list of family maxima (NFR-S5 is correct), and the stages the draft
> calls M8–M13 are **M9–M14** in [`ROADMAP.md`](ROADMAP.md), because M8 was
> already taken. Requirement IDs are unchanged.

What follows is written in `REQUIREMENTS.md`'s own format so that, if the
merge is adopted, this section folds into that document verbatim as
**revision r9** and the tables append to the existing families. Per
`REQUIREMENTS.md` §4, IDs are append-only: new IDs continue each family's
numbering from its current maximum (FR-U3, FR-S8, FR-A8, NFR-D10, NFR-C5,
NFR-I5, NFR-Q5 as of r8), and three new families are introduced (FR-E,
FR-T, FR-M — letters unused by any existing family).

Priority key (MoSCoW): **M** = must have (v2 is not shippable without it) ·
**S** = should have (v2 target, degradable) · **C** = could have
(explicitly designed-for, may slip past v2). Deferred items are in §13.

**v2 = M8–M12** (stages in §14). **Merge-MVP = M8–M9** — the first build
where a work item can open a coding session. "M" priority means required
for v2, not for the merge-MVP; §14 says which stage delivers each ID.

Requirements use *shall*; each is intended to be testable. The Source
column points at the governing section of this document or of the engine's
docs (`docs/engine/` after the merge).

### Revision r9 (proposed) — Vogt runs the work it governs

One non-goal is reversed, deliberately. `DESIGN.md` §1.2 and §3 of the
requirements list *"being an agent runner"* as out of scope: Vogt tells
agents what and why; it does not schedule or execute them. That was the
right boundary for a product that had no execution surface. The merge
gives it one — MyDevEnv2's session engine, adopted whole — and the
boundary moves with the reason for it: **Vogt still never decides to run
anything on its own.** Every session starts because a person (or an
explicitly scheduled agent task a person created) asked. What r3 refused —
the system going looking, continuous checking, autonomous action — stays
refused. FR-G15 is untouched. What changes is only that "start work on
this" is now an operation instead of advice.

Three subordinate decisions, each recorded here because a future reader
will ask:

1. **The engine is adopted as-built, not respecified.** Sessions, PTY,
   attach, scrollback, activity states, agent tasks, push, and the
   assistant arrive with delivered behavior and their own docs
   (`API_CONTRACT.md`, `ASSISTANT.md`, `AGENT_TASKS.md`). FR-E1/E2 and
   FR-T1/T2 state the *load-bearing* properties Vogt now depends on —
   the ones whose regression would break a Vogt requirement — not a
   re-derivation of the engine's full surface.
2. **The write plane is not weakened by the front door.** FR-W1's rule —
   a write needs a reason its author typed — and r6's restatement — a
   mutating operation appears in the GUI only through a view that
   collects one — bind the new surfaces identically (FR-U6, FR-T3).
   The proxy forwards; it never pre-approves (FR-S9).
3. **The assistant's approval gate is a structural guarantee, not
   configuration.** The engine's threat model (no model output can reach
   an effector without an on-screen approval; voice never approves) is
   promoted from module documentation to numbered requirement (FR-T2),
   because the merged product leans on it far harder than MyDevEnv2 did.
4. **The GUI is specified to interaction depth, not surface list.** M6's
   requirements named views (FR-U1) and the delivery was judged by their
   existence. A Jira/Trello-grade GUI fails in its *interactions* — a
   drag that lies about what the server accepted, a filter that resets on
   reload, a list that only updates on refresh — so FR-U10–U22 pin the
   interaction contract itself: liveness, addressability, optimistic
   honesty, keyboard reach, and what every surface does when its data
   source is absent. Each is testable without a pixel being asserted.

### FR-E — Coding sessions & session engine

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-E1 | The system shall run interactive terminal sessions: per-session PTY with ring-buffer scrollback, WebSocket attach with snapshot replay, multiple concurrent clients per session, and full lifecycle (create / list / get / rename / kill / delete). *(Adopted as-built from the engine.)* | M | §5.2, engine README |
| FR-E2 | Each session shall carry a live activity state (`idle` / `running` / `waiting-for-input` / `errored`) derived from output heuristics and published on the server-wide SSE event stream. | M | engine README |
| FR-E3 | The system shall start a session *for a registered project*: the working directory shall be the path the project registry records for it, and template selection shall consult the registry — never path heuristics — so a session opened "for" a project always opens in the registry's tree. The import root and the engine's workspace root shall be the same tree. | M | §6.3 |
| FR-E4 | The system shall start a session *for a work item*: the item's brief (description, `why`, relations) shall be written to a prompt file via the agent-task prompt mechanism; the session id shall be recorded on the work item as an audited write; and the work item's views shall reflect the session's live activity state. | M | §6.1 |
| FR-E5 | Sessions started for a project or work item shall register the Vogt MCP server for agents running inside them, carrying a per-session actor-scoped token, so that agent writes to Vogt are attributed to that session's actor. | M | §6.1, FR-S10 |
| FR-E6 | Session outcomes — exit code, duration, resulting working-tree delta — shall be collected as observations with freshness and trust, like all other evidence. | S | §6.2 |
| FR-E7 | A scheduled agent task may be bound to a project or work item; a bound run's findings shall be recordable as Vogt observations, not only as push notifications. | S | §6.1 |
| FR-E8 | `session.start`, `session.list`, and `session.stop` shall be operations in the registry, and therefore present with parity on CLI, REST, and MCP (FR-A1). | M | §6.2 |
| FR-E9 | The engine shall remain bootable with vogt-core absent, degrading to plain sessions — absence of the core costs Vogt features, never session availability (the ContextKeeper degrade rule, applied inward). | S | §11.2 |

### FR-T — Conversational assistant (the AI layer)

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-T1 | The assistant shall be a server-side tool-use loop with read access to sessions (`list_sessions`, `read_session_tail`) and to a curated read-only slice of the operation registry (at minimum: `backlog`, `bugs`, `why`, `project.brief`, `project.list`, `work.get`, `work.list`, `compliance`); registry-backed tool schemas shall be generated from the registry, not hand-written. | M | §8.1 |
| FR-T2 | Every mutating assistant tool — `send_input`, any `work.*` write, `session.start` — shall pass the pending-action gate: one pending action at a time, carrying the exact payload and target, expiring unapproved, approved only by an on-screen act. No model output shall be able to bypass the gate, and the voice path shall never approve. *(Promoted from `ASSISTANT.md`'s threat model.)* | M | §8.2 |
| FR-T3 | An assistant-initiated Vogt write shall be audited to the approving user's actor with a `why` derived from the conversational context — never to a shared "assistant" actor. | M | §8.2, FR-W1 |
| FR-T4 | Assistant tool results carrying external content — terminal output, forge-derived text, imported issue bodies — shall be delimited as untrusted data; the threat-model rule that external content never becomes instructions extends to every Vogt read. | M | §8.5 |
| FR-T5 | The assistant shall be drivable by voice: push-to-talk STT in the mobile shell, spoken replies in any client, with a validation pass against domain vocabulary (project names, "backlog") before v2 ships — voice is adopted unproven and shall not be presumed working. | S | §8.4, ASSISTANT.md |
| FR-T6 | The assistant shall not exist unless configured: absent its API key the routes answer 404 and every GUI hides the surface. *(As-built rule, retained.)* | M | ASSISTANT.md |
| FR-T7 | The tool loop shall be provider-portable: an OpenAI-compatible backend and a native Anthropic backend shall both be supported, and the currently-documented hang against `claude-*` proxy routes shall be resolved or the route refused with a named reason. | C | §8.4 |

### FR-U — GUI (appended)

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-U4 | The GUI shall present a board of work items whose columns are the workflow's states read from `workflow.list` — never hard-coded — where a drag is a `work.transition` and an illegal transition bounces with the server's stated reason. | M | §7.1 |
| FR-U5 | The GUI shall present a work item in full: description, state history, comments, relations, labels, per-item audit trail, collected evidence with freshness and trust, and the start-session control (FR-E4). | M | §7.2 |
| FR-U6 | The GUI shall present the ranked backlog and bugs views with the explainable `why`, quick-create, and bulk transition/label — under r6's rule: a mutating operation appears only through a view that collects a reason the user typed. | M | §7.3 |
| FR-U7 | The GUI shall present per-project pages: brief, CI status, contract/compliance, drift inbox, dependency graph, and the import form. | S | §7.4 |
| FR-U8 | The PWA shall consume only public APIs, and every URL in the shipped bundle shall resolve against the operation registry *and* the engine's API contract — extending the existing M6 assertion to the Solid bundle and to both halves. | M | §7 |
| FR-U9 | The legacy GUI shall keep serving at `/ui-legacy` until every operation it exposed is reachable in the PWA, and shall then be removed — parity is asserted, not assumed. | S | §5.1 |
| FR-U10 | Views showing server-announced state — session activity (FR-E2), work item state, drift arrivals, notification counts — shall update live from the SSE stream without a manual refresh. A lost stream shall be indicated and shall reconcile on reconnect; a stale view shall never present itself as current. | M | §7, engine README |
| FR-U11 | Every project, work item, board (including its active filter set), session, and audit query shall be addressable by URL: deep links shall survive reload, be shareable, and restore the exact view. Terminal deep links (`/#/t/<id>`) are adopted as-built; Vogt surfaces shall follow the same scheme. | M | §7, engine README |
| FR-U12 | A drag or inline edit shall render optimistically and reconcile against the server's answer, which is authoritative: a refused `work.transition` shall roll the item back visibly and surface the server's stated reason where the drop happened. The client shall never persist, cache, or re-derive a state the server refused. | M | §7.1 |
| FR-U13 | The board shall support swimlanes by project or initiative, per-column WIP counts, and collapse/expand of lanes and columns; lane and column layout preferences shall persist per client. | S | §7.1 |
| FR-U14 | Board, backlog, and bugs views shall filter by project, workflow state, type, label, initiative, and actor, with filters combinable and reflected in the URL (FR-U11). A combined filter shall be nameable and recalled as a saved filter; saved filters are per-client state in v2 (server-side shared filters are deferred, §13). | S | §7.1, §7.3 |
| FR-U15 | Quick-create shall exist on the board and backlog: title, type, project, and the typed reason FR-W1 requires, inline, without leaving the view; every other field is deferrable to the detail view. A quick-create that omits the reason shall not submit. | M | §7.3, FR-W1 |
| FR-U16 | The command palette shall reach every read surface (projects, work items, sessions, views) by fuzzy name, and every GUI-exposed mutating verb by opening the view that collects its reason — the palette itself shall never execute a write directly (r6 rule restated for the keyboard path). | S | §7, web CommandPalette |
| FR-U17 | Trust state and freshness shall be displayed on every aggregated view (FR-U2 extended to the new surfaces), and session-derived evidence shall show the session's activity state as its liveness indicator: a claim backed by a still-running session is marked as provisional, not fresh. | M | FR-U2, §6.2 |
| FR-U18 | The drift inbox shall show each proposal's evidence (both sides of the disagreement, with provenance and age) *before* any act is possible, and accept/reject shall collect a typed reason. Bulk accept shall not exist. | M | §7.4, FR-R |
| FR-U19 | The audit browser shall filter by actor, project, operation, and time range, and every rendered write shall show who, what, and why. A work item's detail view shall link into the audit browser pre-filtered to that item. | S | §7.5, FR-S6 |
| FR-U20 | A work item linked to a session (FR-E4) shall show the live activity badge and an open-terminal control that navigates to the terminal surface attached to that session; the terminal surface shall link back to the owning work item. | M | §6.1, §7.2 |
| FR-U21 | Every surface shall have a designed absent state: engine unavailable → Vogt views work and session controls disable with the named reason; vogt-core unavailable → terminal, files, git, and assistant-over-sessions work and Vogt surfaces report the outage rather than rendering empty data as truth (FR-E9's degrade rule, made visible). | M | §11.2, FR-E9 |
| FR-U22 | The board shall be operable entirely from the keyboard: item focus, moving an item between columns (the drag's equivalent, still subject to FR-U12's reconcile), opening detail, and quick-create shall each have a binding discoverable in the GUI. | S | §7.1 |

### FR-M — Mobile surface

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-M1 | The mobile app shall be the Capacitor shell loading the merged PWA. Its MVP1 feature set shall be: terminal sessions, assistant with voice, push, backlog/board read, and session start/approve. | M | §3, §14 M12 |
| FR-M2 | Push notifications shall be routed for events worth a phone interruption: a session entering `waiting-for-input` or `errored`, new drift, and the agent-task `MYDEVENV2_NOTIFY` hook — and for nothing else by default. | S | §10 |
| FR-M3 | Vogt surfaces shall be usable at phone widths; the board shall render as a list, not columns, below the narrow breakpoint. | S | §7 |

### FR-S — Security, identity & audit (appended)

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-S9 | The front door shall hold the single public token namespace, extended with Vogt capabilities; each front-door token shall map to a named Vogt actor whose paired core token the proxy injects — so audit records real actors, the proxy never pre-approves, and double-gated writes are not weakened. | M | §9 |
| FR-S10 | A session started for a project or work item shall receive a per-session actor-scoped token minted at start and revoked at session end; its writes shall be distinguishable in the audit log from every other actor's. | M | §6.1, §9 |

### NFR — appended

| ID | Requirement | Pri | Source |
|---|---|---|---|
| NFR-D11 | The merged product shall ship as one stack with one published port: the Rust engine is the front door serving the PWA, its native APIs, the WebSocket attach path, `/api/vogt` and `/mcp` proxied to the core, and aggregate health; vogt-core shall bind loopback only and never be published. Any port that serves MCP shall serve plain HTTP health (NFR-D preserved at the front door). | M | §5.2 |
| NFR-D12 | The dev/prod split shall be branch-shaped before any merge phase lands: `dev` builds `:dev` images deployed to a dev stack for live validation; only `main` deploys to prod. *(Adopted from the engine's `uplift.md`, where it is already named a prerequisite.)* | M | §10 |
| NFR-C6 | The merged CI shall run both halves on every push — Rust fmt/clippy/test, web typecheck, APK build, and the existing Python suite — and the build-vs-release discipline (NFR-C3: a push builds `sha-` images, only a tag releases) shall govern the merged image. | M | §5.1 |
| NFR-I6 | Backup and restore shall cover the whole product state as one act: the core's SQLite, the engine's `state_dir`, and enough registry/workspace metadata to re-establish FR-E3's path agreement after restore. | M | §10 |
| NFR-Q6 | Both test suites shall pass in the merged repository, and two absence-modes shall stay green: the forge-less run (NFR-PO1–PO3, untouched) and a core run with no engine present — vogt-core remains fully testable alone. | M | §5.1, FR-E9 |
| NFR-S5 | GUI views shall stay interactive at estate scale — on the order of a hundred projects and a few thousand open work items: long lists virtualize, the board's filter/drag path does not degrade with backlog size, and no view fetches the whole estate to render a page of it (server-side pagination and filtered queries already exist; the GUI shall use them). | S | §7 |

## 13. Explicitly deferred (non-goals for v2)

In `REQUIREMENTS.md` §3 style — named so they are decisions, not
omissions. Items already deferred by r2–r8 (discovery, candidate listing,
per-actor notification inboxes, multi-forge, multi-node) **remain deferred
and are not re-litigated by the merge**.

- **Backend convergence on Rust (§4 option B).** The two-process shape is
  the v2 requirement (NFR-D11); porting the core is a possible future with
  no requirement justified by it — the same discipline r3 applied to the
  AI-drift stretch goal.
- **Autonomous work pickup.** The system shall not start sessions on its
  own initiative — no "agent picks the top backlog item" loop. Every
  session traces to a human act or a human-created schedule (FR-E4/E7).
  This is the surviving core of the reversed non-goal.
- **Voice approval.** Approving a pending action by voice, in any form.
  FR-T2 forbids it for v2; lifting that is a threat-model revision, not a
  feature.
- **Sprint ceremonies, burndown, time tracking.** Vogt's v1 non-goals
  stand; Jira-*shaped* does not mean Jira-complete.
- **Server-side saved filters.** FR-U14's saved filters are per-client in
  v2. Sharing them requires per-user server state, which Vogt does not
  otherwise have and should not grow for this alone.
- **Bulk drift resolution.** FR-U18 forbids bulk accept deliberately: a
  drift acceptance is a declared-state write and carries its own reason.
  Making that convenient in bulk is exactly how r6's rule would erode.
- **GUI-side offline mode.** The PWA installs, but no Vogt surface caches
  data for offline mutation; a queued offline write cannot carry an
  honest freshness answer.
- **The GPUI desktop client.** Stays archived in the MyDevEnv2 repo; not
  carried into the merged tree (§5.1).
- **Cadastre consolidation.** Both "estate registers" keep running as-is;
  their overlap is its own future investigation (§11.3).
- **iOS shell.** MVP1 is Android (the existing shell); nothing designed
  here precludes iOS, nobody builds for it in v2.

## 14. Stages (M8–M13)

> **Folded — no longer authoritative (2026-08-14).** These stages are now
> **M9–M14** in [`ROADMAP.md`](ROADMAP.md), which governs. They were
> renumbered in the fold: M8 was already taken by *Reachable by an agent*
> (FR-A8), so "M7 is the last stage" below was wrong. Read the roadmap for
> the stage list, its cut lines, and the demos as written; this section is
> kept as the working record.

Continuing `ROADMAP.md`'s numbering (M7 is the last delivered stage).
Each stage names the requirement IDs it delivers and ends in a demo that
runs as an acceptance test, per vogt practice. Sizes relative (S<M<L).

**M8 — Foundations (M).** Delivers NFR-D11, NFR-D12, NFR-C6, NFR-Q6,
FR-U9 (legacy GUI at `/ui-legacy`). Repo merge with history (§5.1);
combined CI; `dev-vogt` stack live on Node B; front-door proxy for
`/api/vogt` + `/mcp`; aggregate health. *Demo: one URL serves the PWA, a
terminal session, and a Vogt backlog query; both suites pass in the merged
repo, including the engine-less core run.*

**M9 — Coding sessions (M).** Delivers FR-E1–E5, FR-E8, FR-E9, FR-S9,
FR-S10. Workspace unification; `session.*` operations; work item ↔
session linkage; first-party MCP registration with per-session actor
tokens; auth mapping. *Demo: import a GitHub repo, create a work item,
start an agent session on it, watch the agent update the work item over
MCP, and read the write in the audit log attributed to the session's
actor.*

**M10 — GUI uplift (L).** Delivers FR-U4–U8, FR-U10–U22, NFR-S5, and
retires FR-U9's legacy surface at parity. Build order: board with the
interaction contract first (FR-U10–U12 land with FR-U4, not after it) →
work item detail (FR-U5, U17, U20) → backlog/bugs (FR-U6, U14, U15) →
project pages and drift inbox (FR-U7, U18) → global (audit browser
FR-U19, inbox, admin) → palette and keyboard pass (FR-U16, U22) →
absent-state pass (FR-U21). *Demo: every operation the legacy GUI exposed
is reachable in the PWA; a board drag round-trips `work.transition`
including a rejected transition rolling back with the server's reason; a
board URL with filters restores its exact view after reload; killing the
engine mid-demo disables session controls with a named reason while every
Vogt view keeps answering.*

**M11 — AI layer & voice validation (M–L).** Delivers FR-T1–T4, FR-T6;
FR-T5 validated; FR-T7 attempted. Registry-derived read tools, gated
write set, threat-model extension, provider cleanup, then the deliberate
voice shakedown. *Demo, by voice on the APK: ask for the top bug, hear
the answer, start a session on it, approve by on-screen tap.*

**M12 — Mobile MVP1 (S–M).** Delivers FR-M1–M3, FR-E6, FR-E7. Shell
repointed at the merged PWA; APK CI on the dev stream; push routing;
mobile-fit pass. *Demo, from the phone: receive a push that a session is
waiting for input, open it, unblock it.*

**M13 — Consolidation (S, ongoing).** Delivers no new IDs. Standalone
stacks retired; name/domain decision (§11.1); config alias sunset; and
the r9 revision folded into `REQUIREMENTS.md`/`DESIGN.md` in the usual
as-built reconciliation style — including §5-style delivery verification
of every ID above.

---

## Appendix A — What was examined

- `vogt@main`: `README.md`, `docs/DESIGN.md` (§1 mission, §1.2 non-goals),
  `docs/DEPLOYMENT.md` (process model, Node B/Komodo target),
  `src/vogt/registry/operations.py` (58 operations), `src/vogt/adapters/`
  (http, mcp, cli, github, git), `src/vogt/gui/static/` (~1k lines),
  `src/vogt/application/services/` (19 service modules), storage layout.
- `MyDevEnv2@dev` (head `2214a7d`): `README.md`, `uplift.md` (dev/prod
  split plan), `docs/ASSISTANT.md` (architecture, tools, threat model,
  voice), `docs/AGENT_TASKS.md`, `server/src/app.rs` (full route table),
  `server/src/auth.rs` (capability enum), `server/src/config.rs`
  (session templates, assistant + ContextKeeper config),
  `server/src/contextkeeper.rs` (degrade rules, DNS pin), `web/src/`
  (component inventory), `mobile/` (Capacitor 8 shell), recent `dev`
  history (Vogt MCP registration, ContextKeeper continuity, cadastre
  repoint).
