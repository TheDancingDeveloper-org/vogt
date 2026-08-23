# Open-source transition plan

Working document for the transition described in [`opensource.md`](../opensource.md)
and recorded as revision r20 of [`REQUIREMENTS.md`](REQUIREMENTS.md). Steps are
ordered by dependency, not by schedule — each one names what it needs from the
one before it.

Delete this file once step 8 closes; it is execution material, not product
documentation.

---

## Target state

Three claims, all of which must be true at the end:

1. **The repository delivers a generic Vogt.** One Python-core image built
   from an upstream registry, one Compose base, one configuration schema. A
   stranger can go from clone to running instance with no access to any
   private registry, path, service, or repository.
2. **Customisation is a supported surface, not a fork.** The extension points
   are named and documented, and a deployment that needs more than the base
   states its difference rather than editing the base.
3. **The estate deployment is that base plus configuration**, at parity with
   what runs today, and is documented as the reference customisation rather
   than as the way Vogt is meant to be run.

### What changes structurally

```
TODAY                                TARGET

vogt-stack image                     deploy/vogt.compose.yml   (the base)
├── Rust engine (front door)         └── vogt        ← public image, unchanged
├── Python core   ← its own build
├── Tailscale                        + estate.overlay.yml      (the difference)
├── dev tooling                        └── engine    ← private image, no core
└── Sway, sccache                          VOGT_CORE_URL=http://vogt:8000

one container, one build             one base, one overlay, two containers
```

The mechanism already exists and is requirement-blessed: `VOGT_CORE_URL`
pointed at a non-loopback address makes the engine proxy to a core it does not
run, which `engine/deploy/entrypoint.sh` calls "a legitimate topology", and
NFR-D11 permits "one stack with one published port (**or one two-service
compose, per §5.2**)".

---

## Where this stands

**The target state is live on `vogt-dev`.** It runs
`vogt.compose.yml` + `estate.overlay.yml` + `estate.docker-socket.yml` as two
containers — the engine as front door on `18097`, and the core as a sibling
running **`ghcr.io/…/vogt@sha256:f8903ff6…`, the published public image**.
Instance `ins_01M019AZ8GBNE22FT74Y746JBK` came through intact, `readyz` reports
`vogt_core ready (schema 3 of 11)`, and `vogt backup` writes
`engine_state: copied from …/mydevenv2` rather than "not configured".

The private path is now the public path plus a compose overlay, verifiable by
reading two files rather than by trusting that two builds agreed.

**Merged to `dev`:** #141 (the boundary) · #145 (docs reframed) · #146, #147
(the claude-code stub) · #150, #153 (the estate overlay) · #152 (pins and the
trap) · #154 (`dev` unbroken) · #155 (`latest` stated) · #159 (the `ENGINE_`
prefix).

**In flight:** [#158](https://github.com/TheDancingDeveloper-org/vogt/pull/158)
— the merged image derives its core from the published one by digest. Last
piece of #143.

**Issues closed:** #142 · #138 · #139 · #151 · #148 · #149 · #144.
**Open:** [#143](https://github.com/TheDancingDeveloper-org/vogt/issues/143)
(awaiting #158) · [#156](https://github.com/TheDancingDeveloper-org/vogt/issues/156)
(optional integrations a public reader inherits) ·
[#157](https://github.com/TheDancingDeveloper-org/vogt/issues/157)
(`VOGT_ENGINE_URL` never set, so core-driven sessions have never worked).

**What is left:** step 8 only — making the repository and GHCR packages
public. That needs a person.

---

## What this cost to learn

Recorded because the conclusions are cheap and the corrections are not.

- **Reproduce before diagnosing.** A build broke an hour after a change of
  mine; I blamed the change, merged a fix, and it broke identically. The cause
  was `@anthropic-ai/claude-code@2.1.237` shipping a shell stub. Timing is not
  evidence.
- **Green PR checks are not green CI.** #152 merged clean because path
  classification *skipped* the `core-alone` job for that PR. It ran on `dev`
  and went red. A check that did not run is not a check that passed.
- **Verify the effect, not the intent.** Komodo returning the new
  `file_paths` meant the config had changed, not that the deploy had used it.
  I reported a rollback twice while the stack was still on the old shape.
- **Isolation proves the mechanism, not the deployment.** The overlay was
  "proven" in isolation and still carried eight defects against the real
  stack — volume name, engine state, an assistant gate that would have
  refused to deploy, a port variable, the Docker socket on the wrong service,
  a read-only mount that breaks import, and finally Tailscale breaking
  service-name DNS. Every one presented as a healthy container.
- **Read the comment you are deleting.** The ContextKeeper `extra_hosts` entry
  said *"a landmine if dropped… Tailscale overwrites /etc/resolv.conf and
  breaks Docker's service-name resolution"*. I removed it as inert, correctly,
  then built a topology that depended on exactly that resolution.

---

## Standing gotchas found while executing

- **A PR's closing keyword never fires here.** Everything merges into `dev`;
  GitHub only auto-closes on merges into the default branch (`main`). Close
  issues by hand.
- **A push to the ops repo does *not* deploy.** Komodo reports
  `webhook_enabled: true` for `vogt-dev`, but nothing on the Forgejo side
  sends one — a push left `latest_hash` untouched. `DeployStack` is a
  deliberate act, as the runbook always said. The tell was there before the
  push: `deployed_hash` was already two commits behind `latest_hash` with
  nothing red.
- **The Komodo stack named `vogt` deploys `personal/vogt-prod-candidate`**,
  and `vogt-legacy-core` — which deploys `personal/vogt` — is down.
- **18094 is live, and it is `vogt-prod-candidate`.** An earlier note here
  said nothing served it — wrong, and instructively so: the terminator runs in
  the **host** network namespace (`tailscale serve` → `127.0.0.1:18098`), so
  it never appears in a `docker ps` ports check, and hitting it by IP fails
  the TLS handshake on SNI, which reads like "dead". Traced with a marked
  request: 1 hit in prod-candidate, 0 in vogt-dev. Its advertised
  `VOGT_PUBLIC_URL` is correct.
- **The three ways in are separate.** `winrarhost…:18094` → prod-candidate;
  `vogt-dev.sprooty.com` → Caddy → `100.101.101.57:8910`, the vogt-dev pod's
  *own* tailnet node; `100.92.54.45:18097` → vogt-dev's published port.
  Redeploying vogt-dev cannot affect 18094.

---

## 1. Return CI to the private runner pool

**Needs:** nothing. **Tracking:** this branch. **Done — `c301b85`.**

The branch moved validation to `ubuntu-latest` and revised NFR-C4 into a trust
split. Reviewed and reversed: where a job runs is not what makes an image
generic — the Dockerfile is, and every job that builds or publishes an image
never left the pool. Building from source was never a CI concern either. The
one real exposure is fork-submitted code executing on a tailnet-connected
worker once the repository is public, and that is closed at the gate in step 8
rather than by relocating the pipeline.

- [x] `ci.yml` — all six jobs back to `[self-hosted, node-b, linux, x64]`.
- [x] `docs.yml` — back to the pool.
- [x] `mirror-base-images.yml` `check` — back to the pool. Its `mirror` job
      keeps the `if: github.event_name != 'pull_request'` guard it already had.
- [x] `release.yml` `distribution` — back to the pool, so the release pipeline
      sits on one pool.
- [x] `runner-policy.yml` — the audit job itself back to the pool, and the rule
      returns to "every job names a self-hosted runner". Keep the two fixes
      that are independent of the split: `.yaml` coverage, and the
      expression-syntax fix from `4aa7ad2` without which the workflow does not
      start.
- [x] Drop the fork-guard rule from the gate and from `tests/test_deploy.py` —
      it only made sense alongside the split. Keep `_workflow_files()`.
- [x] `tests/test_deploy.py` back to asserting self-hosted.
- [x] **NFR-C4**: reaffirm the r4 rule, record that the split was considered
      and rejected and why, and state that fork-PR approval is a prerequisite
      of NFR-O1's milestone rather than optional hardening.
- [x] Rewrite r20's decision 3 to match, rather than deleting it — the reasoning
      is worth keeping even though the conclusion changed.
- [x] Note on #141 that the CI section of its description no longer describes
      the branch.

**Done when:** no `ubuntu-latest` appears in any workflow, the gate enforces
that, and NFR-C4 explains the choice. — *Met. 907 tests at 92%; ruff, format,
mypy and the docs link check clean; the gate run verbatim against the tree.*

**Also removes a running cost:** GitHub-hosted minutes are billed on a private
repository, and a full run is roughly 12–13 minutes.

---

## 2. Merge the boundary

**Needs:** step 1. **Tracking:** [#141](https://github.com/TheDancingDeveloper-org/vogt/pull/141) — **merged, `235cc7a`.**

- [x] Merge #141 into `dev`. All nine checks passed on the pool first —
      engine 4m45s, Android 3m7s, both Python matrices, core-alone, docs.
- [x] `runner-policy` green on `dev`, on a **`push`** event — the failure mode
      that was live before `4aa7ad2`.
- [x] `docs` green on `dev`.
- [x] `ci` green on `dev`.
- [x] `build.yml` publishes `vogt:dev` — the **core** image built and pushed.
- [x] `build.yml` publishes `vogt-stack:dev` — `dev-8e04c6e`,
      `sha256:8033b9ce…`, signed. **Failed twice first, and my first diagnosis
      was wrong.** The smoke test caught `exec /usr/local/bin/claude: exec
      format error`; I blamed splitting the npm install and merged
      [#146](https://github.com/TheDancingDeveloper-org/vogt/pull/146) on that
      basis. It failed identically after, which ruled the Dockerfile out. The
      cause was upstream — `@anthropic-ai/claude-code@2.1.237` ships
      `bin/claude.exe` as a shell stub, reproduced in a clean
      `node:22-bookworm` container where 2.1.235 and 2.1.236 are ELF and
      2.1.237 is text. Pinned to 2.1.236 in
      [#147](https://github.com/TheDancingDeveloper-org/vogt/pull/147);
      the general exposure is
      [#148](https://github.com/TheDancingDeveloper-org/vogt/issues/148).
      *Reproduce before diagnosing.*

**Done when:** both images are published and every check on `dev` is green.

**Nothing is deployed by this step.** Publishing an image and moving a
deployment are separate acts (NFR-D10).

---

## 3. Move `vogt-dev` onto the new build

**Needs:** step 2. **Tracking:** ops repo, `personal/vogt-dev`

**Scope: dev only.** `vogt-prod-candidate` is deliberately not touched — not
its digest, not its compose file. Its half of the in-flight ContextKeeper
removal stays uncommitted exactly as found.

**Two stacks, not one — and the Komodo names do not match the directories.**
Read from the Komodo API rather than from the runbook:

| Komodo stack | ops directory | Published on | Advertises | Webhook | State |
|---|---|---|---|---|---|
| `vogt-dev` | `personal/vogt-dev` | `100.92.54.45:18097` (tailnet) | `https://vogt-dev.sprooty.com` | **ON** | running |
| `vogt` | `personal/vogt-prod-candidate` | `127.0.0.1:18098` (loopback) | `https://winrarhost.tailc7d3c.ts.net:18094` | off | running |
| `vogt-legacy-core` | `personal/vogt` | — | — | on | **down** |

**Nothing publishes 18094.** That was `vogt-legacy-core`'s port and it is
down, so the address `vogt-prod-candidate` hands out through `connect` and
`/connection-info` reaches nothing — it is on loopback `18098`. Out of scope
here, but it is why "the prod URL" does not answer.

Three corrections this forced:

- The Komodo stack called `vogt` deploys the **prod-candidate** directory, not
  `personal/vogt`.
- The core-only stack is **retired and down**, not live. Earlier notes here
  describing it as running on 18094 were wrong.
- **`vogt-dev`'s webhook is ON**, so a push to the ops repo's `main`
  redeploys it automatically. `deploy/vogt-stack.komodo.md` says
  "Webhook | **off**" and explains why at length — that document is now
  wrong, and the drift matters: it is the difference between "a push stages a
  change" and "a push deploys". Both ops commits therefore go in one push, so
  there is one redeploy rather than two. The `vogt` stack has no webhook and
  needs an explicit `DeployStack`.

- [x] **ContextKeeper removal landed separately, dev half only** (ops
      `5e8bc75`), verified
      inert first rather than assumed: both running containers report
      `CONTEXTKEEPER_URL=` empty, and the engine answers
      `/api/contextkeeper/*` with 404 when it is unset. The removed lines pin
      a hostname nothing dials and pass a token to a switched-off client. The
      ContextKeeper service itself is healthy and belongs to `mydevenv2-dev`.
- [ ] Bump the digest in `ops/personal/vogt-dev/docker-compose.yml`.
- [ ] **In the same commit**, add `CADASTRE_MCP_ENABLED: "1"`. Neither
      deployed copy has a Cadastre opt-in under either name, and the flag
      defaults to off — without it the pod loses the Cadastre bridge.
- [x] `personal/vogt-prod-candidate` deliberately left alone — decided.
- [x] Both ops commits pushed — `5e8bc75` (ContextKeeper) and `8756956`
      (digest `sha256:8033b9ce…` = `dev-8e04c6e`, plus
      `CADASTRE_MCP_ENABLED: "1"`). Desired state is in the repo.
- [x] **Deployed.** `DeployStack` completed at 02:27:46; Komodo reports
      `deployed_hash = latest_hash = 8756956`. I called this blocked while it
      was still completing — the "no pull happening" reading was a snapshot
      taken in the gap, and I generalised it. The webhook genuinely does not
      fire, so the explicit deploy was needed; the delay was not a fault.
- [x] ~~BLOCKED~~ The webhook does not fire (an
      earlier note here claiming it does was wrong: `webhook_enabled: true`
      only means Komodo would accept one; nothing on the Forgejo side sends
      it). An explicit `DeployStack` was accepted and has sat `InProgress`
      for 45+ minutes. Evidence that this is Komodo's git path, not the
      deploy: no pull is happening — the target digest is absent locally and
      the image store is flat at 88.97 GB — while `RunStackService` and
      `RunProcedure` on other stacks completed at 00:00, 01:00 and 02:00, and
      none of those touch git. Forgejo itself answers
      (`/api/v1/version` → `14.0.5`).

      **Pre-existing, not caused by this work.** Before any of it, Komodo
      already reported `deployed_hash 9828695` against `latest_hash 4ac964e`
      — the stack had sat through a commit without redeploying and nothing
      was red. It has still not seen `8756956`. Repo polling has been broken
      for some time; this deploy is just the first thing to need it.

      Nothing is half-applied: `vogt-dev` runs its previous image, healthy.
- [ ] No `DeployStack` for the `vogt` stack. It has no webhook, so leaving
      its files untouched leaves it exactly where it is.
- [x] Parity walked — see the checklist below.

**Done when:** `vogt-dev` runs the new digest and every parity item passes.

**Rollback:** restore the previous digest and redeploy. The data volume is
untouched by either direction.

**What is deliberately unchanged:** the image still carries claude, codex,
flutter, `theclawbay` and `cadastre[mcp-client]` — all four `INSTALL_*` build
arguments are true on `dev`. The assistant still starts, because the deployed
compose states `MYDEVENV2_ASSISTANT_BASE_URL` explicitly.

---

## 4. Reframe the documentation

**Needs:** step 2. **Tracking:** [#142](https://github.com/TheDancingDeveloper-org/vogt/issues/142) — **done, [#145](https://github.com/TheDancingDeveloper-org/vogt/pull/145) merged.**

`README.md` and `GETTING_STARTED.md` route a public reader to
`docs/DEPLOYMENT.md`, which is 123 lines of estate identity. The fix is
framing, not redaction: presented as the reference customisation, that content
becomes the proof the extension points are real.

- [x] Header on `DEPLOYMENT.md` naming it the reference customisation, linking
      to `CUSTOMISATION.md`, and pointing the generic path at
      `GETTING_STARTED.md`.
- [ ] Annotate each estate-specific choice with the extension point it
      exercises — tailnet-only exposure is `ports` + `VOGT_BIND_IP`, the estate
      mount is `import_root` plus uid ownership, the engine is `fronted`.
- [ ] Correct the one-line descriptions in `README.md` and
      `GETTING_STARTED.md`.
- [ ] Fix `USER_GUIDE.md`'s five stale MyDevEnv2 references — those are wrong
      product identity, not customisation examples.
- [x] Confirm nothing being published is a credential — scanned; none. Only
      topology, paths and CLI flags. The two credentials that matter are
      brokered as files.
- [x] `scripts/check_docs.py` clean.

**Done when:** no document a public reader is routed to presents estate
infrastructure as the supported generic path.

---

## 5. Make the reference customisation real

**Needs:** step 3 (a working baseline to compare parity against).
**Tracking:** [#143](https://github.com/TheDancingDeveloper-org/vogt/issues/143) — **deployed and live; only [#158](https://github.com/TheDancingDeveloper-org/vogt/pull/158) outstanding.**

`CUSTOMISATION.md` claims the estate deployment is built from the extension
points. `engine/Dockerfile` still has a `core-build` stage, so today that is
asserted rather than verifiable. Two independent parts; the first stands alone.

### 5a — the overlay

- [x] `deploy/estate.overlay.yml` — adds the `engine` service, states
      differences only, never names an image for the core.
- [x] `vogt` takes the workspace mount, read-only — the **core** is what
      reads repositories for collection.
- [x] `engine_state_dir` resolved by the narrow bind (option 2 of the three
      on #143), and the failure mode reproduced first: with it unset the
      engine reported `backup_agreement — VOGT_ENGINE_STATE_DIR is not set
      here`.
- [x] `engine` gets `VOGT_CORE_URL=http://vogt:8000`, the tailnet port,
      `NET_ADMIN`, `/dev/net/tun`, the three host mounts and every
      `MYDEVENV2_*` / `TAILSCALE_*` / `INFISICAL_*` variable. `ports: !reset
      []` on the core, because Compose concatenates `ports` — verified.
- [x] **Topology proven in isolation** — engine and core as separate
      containers, core from the published image: `vogt_core ready (schema 3 of
      11)` across the network, the engine logging "proxying to a core this
      container does not run", and `/api/vogt` failing with `front-door token
      "primary" has no paired vogt-core token` (the `vogt_core_token` secret).
- [x] **Deployed to `vogt-dev` and parity-walked** — 9 of 10 items verified.
      The CLI moved to the core container as expected.
- [ ] `engine/Dockerfile` derives its core rather than building one — #158.
- [ ] Re-walk the parity checklist against the overlay.

### 5b — the image

- [ ] Delete `core-build` from `engine/Dockerfile`; the engine image ships no
      core.
- [ ] If the merged single-container image is still wanted as a convenience,
      derive it with `COPY --from=ghcr.io/…/vogt@sha256:<digest> /opt/vogt`,
      which needs the public image to emit a relocatable `/opt/vogt`.

**Done when:** `docker compose -f deploy/vogt.compose.yml -f
deploy/estate.overlay.yml up -d` produces the estate deployment, running the
published public image for its core, at parity.

**Rollback:** the merged image and `vogt-stack.compose.yml` remain deployable
throughout; this step is additive until the digest is switched.

---

## 6. Settle the deferred decisions — **done, #144 closed**

**Needs:** step 5 for the second item. **Tracking:** [#144](https://github.com/TheDancingDeveloper-org/vogt/issues/144)

- [x] **`MYDEVENV2_*` compatibility window — the split is done.** 46 names,
      not the 26 the deployed compose shows: 8 on-the-wire or persisted
      (`NOTIFY` is a stdout marker, `SESSION*` and `AGENT_TASK_*` are injected
      into every PTY, `ANDROID_APP_ID` is persisted in installed apps and FCM
      registrations), 31 deployment config that can move behind a fallback
      alias, 4 CI secret names, 3 test-only. Posted with evidence on
      [#144](https://github.com/TheDancingDeveloper-org/vogt/issues/144#issuecomment-5349766441).
- [x] **Decided and implemented** (#159): 21 engine settings are `ENGINE_*`
      with dual-accept in one lookup; group A keeps its names because they are
      protocol and persistence, and that is the answer rather than a deferral.
- [x] **Where the engine lives** — here, without building a core (#158).
- [x] **Tagging policy** — stated rather than inherited (#155).

**Done when:** each is a recorded requirements revision, not a paragraph
saying it is open.

---

## 7. Clear the open issues

**Needs:** nothing for most. **Three closed, two in a PR, two are decisions.**

| Issue | State |
|---|---|
| [#138](https://github.com/TheDancingDeveloper-org/vogt/issues/138) GUI badge polling | **Closed.** Already fixed by `73e2af0`; confirmed live — 8 `limit=1` polls since the redeploy against 2,643 in 3,000 lines before. |
| [#139](https://github.com/TheDancingDeveloper-org/vogt/issues/139) standardised logging | **Closed.** Delivered by `9f8f5dc`; all four acceptance criteria verified live, including the hard one — five request ids correlated across engine *and* core with matching status and duration. |
| [#151](https://github.com/TheDancingDeveloper-org/vogt/issues/151) `main` unmergeable | **Closed.** Required context repointed from `runner-policy / runner-policy` to `runner-policy`; every other protection setting read and written back unchanged. |
| [#148](https://github.com/TheDancingDeveloper-org/vogt/issues/148) unpinned npm globals | [#152](https://github.com/TheDancingDeveloper-org/vogt/pull/152) — all three pinned as annotated ARGs, Renovate `customManager` added and its regex verified against the Dockerfile. |
| [#149](https://github.com/TheDancingDeveloper-org/vogt/issues/149) green check exits 1 | [#152](https://github.com/TheDancingDeveloper-org/vogt/pull/152) — defaulted expansions in the `EXIT` trap. |
| [#143](https://github.com/TheDancingDeveloper-org/vogt/issues/143) overlay | Overlay merged ([#150](https://github.com/TheDancingDeveloper-org/vogt/pull/150)). Remaining: deploy it, and decide `core-build`'s fate. |
| [#144](https://github.com/TheDancingDeveloper-org/vogt/issues/144) three deferrals | Recommendations posted for all three; each is a yes/no from being a requirements revision. |

- [x] #138 — verified and closed.
- [x] #139 — verified and closed.
- [x] #151 — fixed and closed.
- [x] #148, #149 — merged in #152, confirmed by a green image build.
- [x] #143 — overlay deployed and live; `core-build` decided (derive) and
      implemented in #158.
- [x] #144 — three answers, all implemented. Closed.

**Done when:** nothing open describes a defect a new reader would hit, and
whatever stays open is deliberate.

**Two closed issues were already fixed and simply never closed** — #138 and
#139, both delivered by #140. Worth knowing as a habit: this repository merges
into `dev`, so no PR ever auto-closes anything.

---

## 8. Go public


> **This step is not mine to take.** Making the repository and its GHCR
> packages public is outward-facing and effectively irreversible — published
> content stays published. It needs an explicit decision, not inference from
> "execute the plan". The same goes for the two decisions left in step 6:
> analysis is done, choosing is not a task.


**Needs:** steps 2, 4, 6 and 7. **Tracking:** NFR-O1

- [ ] **Require approval for fork pull request workflows from outside
      collaborators**, *before* flipping visibility. This is the control that
      replaces the runner split reverted in step 1: it decides whether
      untrusted code runs at all, rather than where it runs. Not optional
      hardening — the self-hosted pool is tailnet-connected.
- [ ] Repository public.
- [ ] GHCR package public — until then the base's `image:` default cannot be
      pulled and `deploy/vogt.build.yml` is the only working path.
- [ ] `runner-policy` set as a required status check on `main`. A reporting
      gate nobody must pass is a red X anyone can merge past.
- [ ] Verify the quickstart from a machine with no estate access and no
      authenticated registry.
- [ ] Confirm the cosign verification instructions work for an unauthenticated
      caller.

**Done when:** a stranger can go from clone to running instance following
`GETTING_STARTED.md` alone.

---

## Parity checklist for `vogt-dev`

Walk this after step 3, and again after step 5, against `vogt-dev`. It is
the definition of "we did not break dev".

- [x] Front door answers — `/healthz` 200, `readyz` reports `tailscale running and online`.
- [x] Proxy works — `readyz` reports `vogt_core ready (schema 3 of 11)`; `/api/vogt/*` returns 401 (auth layer) rather than 502, and `/mcp` returns 405 on GET, both correct.
- [ ] **Not exercised.** Would need creating a session. The engine's session code is unchanged by this deploy and `readyz` reports `workspace_root` and `state_dir` healthy, but that is inference, not a test.
- [ ] **Not exercised** — same reason.
- [ ] **Partially.** The #141 risk here was `assistant_base_url` becoming a startup error; the engine loaded and is healthy, so that is cleared. A live assistant round-trip was not run.
- [x] **`ok: Cadastre MCP (…:18092/mcp)` — probed, not skipped.** This was the
      real test of the `CADASTRE_MCP_ENABLED` rename, and it passes. Every
      other service `ok` too. The command still exits 1 on a pre-existing
      trap bug, filed as
      [#149](https://github.com/TheDancingDeveloper-org/vogt/issues/149).
- [x] Registered for both claude and codex, `✔ Connected`, alongside cadastre — confirming the bootstrap-gating fix from the review.
- [x] `vogt status` answers, same `instance_id ins_01M019AZ…` as before the deploy — data intact.
- [x] `/` returns 200 with a document. Individual routes not clicked through.
- [x] `readyz` asserts it directly: `backup_agreement — vogt backup covers
      /home/sprooty/.local/share/mydevenv2`. A restore rehearsal is still
      worth doing before step 5 splits the containers.

---

## Decisions still needed

| Decision | Blocks | Note |
|---|---|---|
| Does the merged single-container image survive step 5, or is two-service the only supported shape? | 5b | Two-service is simpler and already blessed; the merged image is convenience. |
| `MYDEVENV2_*` window and alias mechanism | 6 | Nothing persists the newest flag, so renaming is free right now and a migration afterwards. |
| Engine's long-term home | 6 | Answer after 5b, not before. |
| Public image tagging | 6, 7 | Must stay digest-verifiable. |

---

## Standing rules

- Publishing an image never moves a deployment; a digest or configuration
  change does (NFR-D10).
- The ops repository holds the deployed copy; the product repository holds the
  reviewed source of it. Change it here first.
- No hand-edits to a running container, in either direction, including
  rollback.
- Every workflow job runs on the self-hosted pool. The exposure a public
  repository creates is closed by gating fork pull requests, not by moving
  work off the pool.
- `docs/REQUIREMENTS.md` records every decision that changes the architecture
  or the product contract, as a revision rather than an edit.
