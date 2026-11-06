# Vogt — current state, target state and roadmap

*Last reviewed 2026-11-06.*

Vogt is partway through two structural changes. The Python core is being
replaced by a Rust core, and sessions are moving from processes inside the
engine's container to one Kubernetes pod each. This document says where each
change stands, what the finished shape is, and which work items carry the
remaining steps.

How it relates to the other documents:

- [`ARCHITECTURE.md`](ARCHITECTURE.md), [`ENGINE.md`](ENGINE.md) and
  [`DEPLOYMENT.md`](DEPLOYMENT.md) describe what is built **on the line you
  have checked out**. They don't describe plans.
- This document covers both lines, the target, and the order of work. It
  holds structure and decisions, not status: the live state of a step is its
  work item in Vogt (`WI-n`), grouped under the initiatives named in §5.
- Designs for single features are in [`design/`](design/).
- A deployment's own rollout (which hosts, which cluster, when) is that
  operator's record and is not kept in this repository.

Keep it current. A change that alters a row in §1, finishes a step in §5 or
settles a question in §6 updates this file in the same change.

---

## 1. At a glance

| | Released (`main`, v0.7.27) | Integration line (`rust-2`) | Target |
|---|---|---|---|
| **Core** | Python `vogt serve`. It is the product authority. | Rust `vogt-core` beside the Python core. `VOGT_CORE_IMPL` picks one; the default is `python`. | Rust core only. The Python core is removed. |
| **Engine** | Rust `vogt-engine`: front door, sessions, agent tasks, assistant. | The same, plus a pod backend and an in-pod session agent. | The same. |
| **Where a session runs** | A PTY inside the engine's container. | Per template: a PTY in the container, or a Kubernetes pod. | A pod per session. The local PTY backend is removed last. |
| **A session's workspace** | A worktree or clone in the workspace folder on the engine's host, shared by every session. | The pod's own clone of one repository, at `/work`. | A clean clone per session, with the workspace-level agent configuration mounted read-only. |
| **Deployment** | One `vogt-stack` container and the voice sidecar, run by Compose. | That, or a Kubernetes Deployment (engine, core and PWA in one pod) with session pods in a second namespace. | The Kubernetes shape for pod sessions. What the single-host Compose shape runs is open (§6). |
| **Source control** | Git throughout. A project's landing policy can name graft, but only as text in the brief. | Pods clone with graft only; everything else is git only. | A project has a source, git or graft, behind one provider interface. Git is the default. |
| **How a change lands** | graft line `backlog-1`, batched into a pull request to `main`. | graft line `rust-2`, rebased onto `main` when needed. | One line. |

The two changes are developed together on `rust-2` and reach a deployment
together: a cutover moves it to the Rust core and to pod sessions in one step.

---

## 2. Released state (`main`)

What `main` ships is described in full by [`ARCHITECTURE.md`](ARCHITECTURE.md)
and [`DEPLOYMENT.md`](DEPLOYMENT.md). In short:

- **One container.** `vogt-engine` (Rust) is the front door and serves the
  PWA. `vogt serve` (Python) is the core, on loopback behind it.
- **The Python core is the authority** for the operation registry. The CLI,
  REST and MCP surfaces are generated from it.
- **Sessions are local.** Each one is a PTY the engine owns, in the same
  container and the same filesystem as every other session.
- **Work happens in a shared workspace folder.** A session gets a git
  worktree or a graft clone under it, and can read everything else there.
- **Hibernation, autopilot, oversight sessions, context rotation and the
  approval flows** all exist and are written against local sessions
  ([`design/`](design/)).

Nothing on `main` runs a session anywhere but the engine's own container.

---

## 3. Integration line (`rust-2`)

`rust-2` is a graft line, not a git branch of the public repository. It
carries the Rust core and the pod backend, and it is what the maintainers'
dev deployment runs.

### 3.1 What exists

- **`engine/core`** — the `vogt-core` binary: the registry, both SQLite
  stores, the HTTP, CLI and MCP adapters, collectors and forge adapters,
  ported from Python against a black-box parity harness. It embeds the same
  migration SQL as the Python core.
- **`engine/session-agent`** — `vogt-session-agent`, the process that owns
  the PTY inside a session pod and dials the engine.
- **`engine/server/src/pod_backend.rs`** and its neighbours — the pod
  backend: pod rendering, the bootstrap credential, the agent stream,
  reconciliation, per-session graft tokens, Claude account bundles.
- **`engine/Dockerfile.pod`** and **`engine/deploy/session-entrypoint.sh`** —
  the session image and its boot sequence.
- **Documents that exist only there:** `docs/rust-core-gap-register.md`,
  `docs/rust-port/` (the swap plan, accepted differences, HTTP decisions,
  drift triage) and `docs/PYTHON_TEST_DISPOSITION.md`.

### 3.2 What has been shown to work

On the dev deployment, 2026-11-06 (WI-3169, WI-3163):

- The stack runs with `VOGT_CORE_IMPL=rust`: the Rust core, the engine and
  the PWA in one pod.
- Six Claude sessions started as pods with a task and autopilot on. All six
  began their task with no input from anyone, and none showed a trust prompt.
- A session is created in about 0.1 s. A warm pod reaches a working agent
  turn in about 17 s; the first batch on a cold node took about 43 s to start
  its containers.
- An idle Claude pod uses about 200 MiB.
- Attach, input, resize, stop, and hibernate then wake of a pod all work.

### 3.3 What has not

- **The Rust core is not ready to be the only core.** The blocking gaps are
  listed in priority order in `docs/rust-core-gap-register.md` on `rust-2`,
  and in §5.1 here.
- **`rust-2` is behind `main` in schema and operations.** `main` carries
  declared migrations 0029 and 0030 and the operations `work.land`,
  `workspace.clean` and `workspace.list`; `rust-2` has none of them. The Rust
  core refuses a database that holds a migration it doesn't know, so today's
  `rust-2` cannot open a database from the current release (WI-2588).
- **Only Claude pods are proven.** The Codex template was refused on dev
  (WI-3335), and neither Codex nor opencode pods are given a first prompt
  (WI-3389).
- **A woken Claude pod resuming its conversation** has not been shown
  (WI-3158).
- **An engine restart loses pod sessions.** The pods are deleted and the
  sessions come back hibernated (WI-3205).
- **Spill-over to a second node** is configured but not observed (WI-3343).
- **Every pod clones the same repository**, whatever project the session is
  for (WI-3369).
- **Pod sessions get no workspace-level rules, skills or tools** (WI-3374).
- **No pipeline builds `rust-2`.** The dev images are built from a clone by
  hand (WI-3386).
- **`rust-2` has no route to `main` yet.** Nothing owned that step until
  WI-3387.

### 3.4 Where `rust-2`'s own documents are behind

Several of `rust-2`'s documents predate changes already on `main`. Its
`AGENTS.md`, `CONTRIBUTING.md` and `ci.md` still describe landing on an
integration branch, which graft replaced, and its version strings read
0.7.22. §7 lists each document.

---

## 4. Target architecture

```text
   browser / installed PWA / Android shell / CLI / MCP clients
                          |
  engine pod  (one Deployment; state on a volume)
    vogt-engine   front door, session records, agent stream, credential broker
    vogt-core     Rust core on loopback: registry, stores, collectors
                          |
                          |  Kubernetes API: create, delete and list pods
                          v
  session pods  (their own namespace, restricted Pod Security Standard)
    session-entrypoint  ->  exchange bootstrap credential  ->  clone  ->
    vogt-session-agent  (one PTY, one agent CLI; dials the engine,
                         never accepts a connection, never calls the cluster)
```

### 4.1 The core

- **A drop-in.** `vogt-core` listens on the same loopback port and answers
  the same HTTP, MCP and CLI contracts, over the same SQLite files. The
  engine, the PWA and a deployed database do not change at the swap.
- **Parity is the acceptance test.** A chunk of the port is accepted against
  the parity harness (`scripts/parity.py` on `rust-2`), with goldens recorded from the
  Python core.
- **The switch is a setting, not a merge.** The stack image carries both
  cores and `VOGT_CORE_IMPL` selects one at container start (WI-2660).
  Changing it and redeploying is the cutover and the rollback.
- **The Python core is removed after the Rust core has soaked** (WI-2505),
  and the documents are rewritten for a Rust-only core at that point
  (WI-2506).

### 4.2 Sessions as pods

- **Create.** The engine renders a pod from the session's template: image,
  resources, placement, labels. It creates the pod and then a Secret, owned
  by that pod, holding a single-use bootstrap credential.
- **Boot.** The entrypoint exchanges the bootstrap credential for a session
  token, the agent's account bundle, the brokered environment, the MCP
  configuration and a graft token. It clones the project, writes the brief
  and hands over to the session agent.
- **Stream.** The session agent opens one outbound WebSocket to the engine
  and carries PTY output, input, resize and signals over it. It keeps a
  replay buffer and a cursor, so a dropped connection resumes without a gap.
- **Hibernate and wake.** Hibernate syncs the transcript and deletes the pod.
  Wake creates a new pod under the same session id and resumes the
  conversation.
- **Reconcile.** Every 15 s the engine compares its sessions with the pods
  that carry its own `vogt.engine` label. It ends a session whose pod failed
  or vanished and deletes a pod no session owns. Engines sharing a namespace
  never touch each other's pods.

The reference for all of this is "Pod-backed sessions" and "Session pod boot"
in `docs/ENGINE.md` on `rust-2`.

### 4.3 Isolation between sessions

Local sessions share a container and a filesystem. Pod sessions don't, and
the design keeps it that way:

- A session pod runs as a non-root user with every capability dropped, no
  privilege escalation, the default seccomp profile, and no service account
  token.
- **No shared writable volume.** The engine refuses to create a pod with one
  (WI-3242). Tool caches are a per-pod `emptyDir`; a shared cache is mounted
  read-only.
- **Credentials are files, never pod-spec values.** The bootstrap credential
  is a Secret volume; everything else arrives in the bootstrap response.
- **A graft token per session**, scoped to one repository and line, minted
  at pod create and revoked at teardown.
- **Claude accounts are bundles leased to sessions**, with a cap per account.
  A credential the CLI rotates inside a pod is written back to the engine.

### 4.4 The workspace

Decided 2026-11-06 (WI-3364):

- **A session works from a clean clone**, made at boot and gone with the pod.
- **The workspace-level agent configuration** (the root `AGENTS.md` and
  `CLAUDE.md`, shared rules, skills and tools) is still needed in every
  session. It is mounted read-only, and the clone sits under it so the agent
  CLIs find it the way they do today (WI-3373, WI-3374).
- **There is no shared working tree.** A session reaches another repository
  by cloning it (WI-3372), and reaches another session's work through a
  landed changeset or a Vogt comment.

This drops three things local sessions have, each with an open item: work
that was never landed surviving a stop (WI-3370), Claude's per-project memory
(WI-3371), and sessions for projects that don't land through graft (WI-3372).

### 4.5 Source control: one interface, two providers

Decided 2026-11-06 (WI-3392). The maintainers land their own work through
graft, so graft is the primary mechanism there. Git stays fully supported and
is the default for anyone else. Vogt itself names neither outside a provider.

Today each is wired into a different set of code paths. Session pods on
`rust-2` can only clone with graft. Local sessions, the engine's git routes,
the PWA's git views, the collectors and the tracker's delivery evidence only
know git. The target:

- **A project has a source:** its kind (`git` or `graft`), where it lives,
  its line or branch, and how work lands. It is defined once, in the
  `contract` crate that the core and the engine both use.
- **The core owns it and passes it on.** The project record holds the
  source. The core gives it to the engine when a session starts, and the
  engine gives it to the pod.
- **A provider does six things:** check out into a folder; mint and revoke a
  scoped credential; show status, diff and log; save unlanded work; land;
  report what landed.
- **Git and graft are the two providers.** Both run in the pod smoke. The
  tracker reads "what landed" through the provider, so work landed through
  graft shows as delivered (WI-3393).

A pod cloning its session's project (WI-3369), repositories that land by git
and a second repository (WI-3372), and saving unlanded work (WI-3370) are
built on this interface, not as separate cases.

### 4.6 The core and the engine stay two programs

The port replaces the Python core with a Rust one behind the same loopback
HTTP contract. It does not join the core and the engine. After the swap they
are two Rust programs in one Cargo workspace that share no types: on `rust-2`
today the core does not depend on the `contract` crate.

That split is why knowledge goes missing between them. The core knows a
session's project; the engine does the clone; nothing carried one to the
other. Sharing the `contract` crate, starting with the source descriptor
above, is the first step toward one codebase. Whether to go further and merge
them into one process is open (§6).

### 4.7 Placement and caches

- **Placement is deployment configuration:** preferred nodes with weights,
  excluded nodes and tolerations, merged with a template's own policy
  (WI-3343).
- **Build caches** are a shared object-store cache plus per-node read-only
  seeds (WI-3164). Choosing a node's cache at schedule time, so caching and
  spill-over work together, is WI-3354.

---

## 5. Roadmap

Work is tracked in Vogt, not here. This section gives the order and the
items that gate each step. Counts are open items on 2026-11-06; about 22 of
them are built and waiting to be closed (WI-3383).

| Initiative | Slug | Open |
|---|---|---|
| Rust core port | `rust-core-port-replace-the-python-core-with-a-drop-in-rust-process` | 34 |
| Vogt as a Kubernetes session orchestrator | `vogt-as-a-kubernetes-session-orchestrator` | 60 |
| GUI responsiveness (after the port) | `gui-responsiveness-server-compute-rail-payload-terminal-renderer-after-the-rust-port` | 16 |
| Agent harness services | `agent-harness-services` | 24 |
| Event-driven oversight and structured worker results | `event-driven-oversight-and-structured-worker-results-harness-review-2026-10-14` | 21 |
| Shared language servers | `shared-language-servers-python-rust-java` | 6 |

The first two are the structural changes. Streams 5.1 to 5.3 run in parallel
on `rust-2`; 5.4 and 5.5 follow in order.

### 5.1 Rust core ready to switch

First, `rust-2` is brought level with `main`: the missing migrations and
operations, and goldens re-recorded by CI (WI-2588, WI-1081).

Then the blocking gaps, in the gap register's order. The second column is
what the code on `rust-2` showed on 2026-11-06; nothing was built or run for
it, and the items are still open in Vogt.

| Gap | Seen in the code |
|---|---|
| **WI-2660** — the `VOGT_CORE_IMPL` gate, so merging is not the cutover | The switch is in the entrypoint, the image and the Compose file. No pipeline builds and smokes an image with `rust` selected. |
| **WI-2021** — the in-process schedule | Collector sweeps and budget-stop settlement run. Watch evaluation and the forge-posture loop are not ported (WI-3385). |
| **WI-2663** — dispatch must not block the async workers; bound the `/mcp` body | Built. |
| **WI-2664, WI-2665** — operations that were stubbed | Charters, watches, `session.report`, `usage.board` and `instance.diagnostics` are built. The inbox does not read session reports yet. |
| **WI-2662, WI-2667** — parity green at the tip, with engine-backed steps for the session and chat operations | The steps exist. Whether parity is green can't be told without running it, and the Rust CLI transport is not checked in CI. |
| **WI-2022, WI-2024** — session digest rows; the `session.start` model policy | Digest rows are built. The model policy is not started. |

Then the items that degrade rather than block (WI-2670 to WI-2673), and the
delivery gate for the line itself (WI-2661). The parent is WI-2659.

Two steps had no item until the 2026-11-06 backlog review (WI-3383): a
pipeline that builds `rust-2` and smokes it with the Rust core selected
(WI-3386), and landing `rust-2` on `main` with the default still `python`
(WI-3387).

### 5.2 Sessions as pods

In the order the operator set: workers, then reviews, then oversight, then
shells and task runs.

1. **Worker and review sessions** — WI-3169, with WI-3162 (session images
   per project) and WI-3389 (Codex and opencode pods). The first prompt, the
   trust prompt and a git view of the clone are done (WI-3355, WI-3356).
2. **Lifecycle** — WI-3158 (hibernate and wake with a resumed conversation),
   WI-3160 (credential and transcript write-back), WI-3205 (re-adopt pods
   after an engine restart), WI-3245 (teardown race).
3. **Security** — WI-3242 (no shared writable cache), WI-3243 (the permission
   posture inside pods), WI-3248 and WI-3249 (cluster role and egress
   narrowing).
4. **Placement and caches** — WI-3343, WI-3354, WI-3164.
5. **Oversight and interactive sessions** — WI-3170.
6. **Shells and agent-task runs, then delete the local PTY backend** —
   WI-3171.

### 5.3 The workspace in the pod model

Parent WI-3364.

- **Audits first:** WI-3365 (unlanded work in existing session copies),
  WI-3366 (files outside any repository), WI-3367 (what sessions read outside
  their own checkout), WI-3368 (which projects can run in a pod).
- **The source provider interface first:** WI-3392. Three items below are
  built on it.
- **Build:** WI-3369 (a pod clones its session's project), WI-3373 and
  WI-3374 (the read-only workspace configuration), WI-3372 (repositories that
  land by git, and a second repository), WI-3370 (unlanded work), WI-3371
  (agent memory), WI-3393 (the tracker sees work landed through graft).

### 5.4 Cutover of a deployment

A deployment moves when 5.1's blocking gaps are closed and 5.2 covers the
session kinds it uses. The plan and its checklist are
`docs/rust-port/swap-release-plan.md` on `rust-2` (WI-2507): flip the core,
soak, keep the way back. That plan predates the Kubernetes shape and needs a
refresh; the note is on WI-2507. The maintainers' own production cutover is
WI-3384.

### 5.5 The swap in the repository

After a deployment has soaked on the Rust core: rewrite the documents for a
Rust-only core (WI-2506), remove the Python core and rename `vogt-core` to
`vogt` (WI-2505). The parent is WI-1082.

---

## 6. Decisions and open questions

### Decided

| Date | Decision |
|---|---|
| 2026-10-08 | The Rust core is a drop-in for the `vogt serve` process, built on a side line and accepted against a parity harness; one swap, not a gradual migration. |
| 2026-10-28 | Holistic review of the port: keep building, not ready to swap. Merging must not be the cutover (WI-2659, WI-2660). |
| 2026-11-03 | Every session becomes a pod: workers, reviews, oversight, then shells and task runs. The local PTY backend is deleted last. |
| 2026-11-03 | The Rust core and the pod work land on one line, `rust-2`, rebased onto `main` when needed. |
| 2026-11-03 | Workspaces are ephemeral: clone at boot, land through graft. |
| 2026-11-03 | Claude accounts are bundles the engine leases to pods, with write-back. |
| 2026-11-06 | Session pods prefer one node and spill over to others; placement is deployment configuration (WI-3343). |
| 2026-11-06 | No shared working tree. A clean clone per session, plus the workspace-level agent configuration mounted read-only (WI-3364). |
| 2026-11-06 | Graft is the maintainers' primary source-control mechanism; git stays supported and is the default; graft is not hard-coded. Both sit behind one source provider interface (WI-3392). |

### Open

- **The single-host shape.** Once the local PTY backend is gone (WI-3171), a
  Compose deployment with no cluster has nowhere to run a session. Nothing
  read for this review records what that deployment does: keep a local
  backend for it, require a single-node cluster, or something else.
- **One program or two.** After the swap the core and the engine are both
  Rust but still separate programs joined by HTTP (§4.6). Whether to merge
  them into one process is undecided, and no work item covers it. It is a
  question for after the swap; sharing the `contract` crate comes first
  either way (WI-3392).
- **Two pods on one Claude account.** Whether an OAuth refresh in one pod
  signs the other out (WI-3152).
- **Unlanded work when a pod stops.** Save it as a draft, or refuse to stop a
  dirty session (WI-3370).
- **Agent memory.** Carry it per project, or retire it in favour of Vogt
  (WI-3371).
- **How the workspace configuration reaches a pod.** A read-only network
  volume, an image layer, or an image volume (WI-3373).
- **A second repository.** Declared when the session starts, or granted on
  request (WI-3372).

---

## 7. Documentation map

Which document to trust for what. "Behind `main`" means the `rust-2` copy
predates changes already on `main`; the count is differing lines on
2026-11-06. Documents were compared between the lines, not audited sentence
by sentence.

| Document | On `main` | On `rust-2` |
|---|---|---|
| `AGENTS.md` | Current. | Behind `main`: describes the integration-branch landing flow that graft replaced. |
| `README.md` | Current. | Near identical. |
| `docs/ARCHITECTURE.md` | Current for the released product. | Identical to `main`, so it describes neither `vogt-core` nor pod sessions. |
| `docs/ENGINE.md` | Current for local sessions. | Adds "Pod-backed sessions" and "Session pod boot"; otherwise behind `main` (649 lines). |
| `docs/DEPLOYMENT.md` | Current (0.7.27). Compose only. | Adds `VOGT_CORE_IMPL` and the pod settings; version strings behind (0.7.22). Neither line has a Kubernetes chapter. |
| `docs/CONTRIBUTING.md`, `docs/ci.md` | Current. | Behind `main` (90 and 102 lines). |
| `docs/AGENT_GUIDE.md`, `docs/API.md` | Current. | Behind `main` (178 and 100 lines). Not checked for what changes in a pod. |
| `docs/USER_GUIDE.md`, `docs/CONFIG.md`, `docs/CUSTOMISATION.md` | Current. | Small differences. `CONFIG.md` is generated. |
| `docs/GETTING_STARTED.md`, `docs/SCHEMA.md`, `docs/GITHUB_APP.md` | Current. | Identical. The schema is shared by both cores. |
| `docs/design/*` | Feature designs, written against local sessions. | The same files. None records what changes for a pod session. |
| `docs/rust-core-gap-register.md`, `docs/rust-port/*`, `docs/PYTHON_TEST_DISPOSITION.md` | Absent. | The Rust port's own record. |

The work to close these gaps: WI-3377 (bring `rust-2`'s process documents up
to `main`), WI-3378 (as-built documents on `rust-2`), WI-3379 (a Kubernetes
chapter in `DEPLOYMENT.md`), WI-3380 (pod-session behaviour in the feature
designs) and WI-2506 (the rewrite for a Rust-only core, at the swap).

Until the documents on `rust-2` catch up, read them this way:

- **Working on `main`:** the documents there are right for the code there.
  Use this file for where the code is going, so a change doesn't deepen a
  dependency on the Python core or on a shared working tree without saying
  so.
- **Working on `rust-2`:** trust `ENGINE.md`'s pod sections, the gap
  register and `docs/rust-port/`. For how changes land, CI and anything else
  about process, read `main`'s copy.
