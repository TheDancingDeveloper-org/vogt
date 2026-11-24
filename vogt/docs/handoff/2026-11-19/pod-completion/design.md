# vogt-dev: finish the pod session backend (WI-4426)

Design and work plan, 2026-11-19. Written by session ses_01M7V0J5STKVTN827G0JCR5AZ5.

**Scope (operator, 2026-11-19).** On vogt-dev, every worker and review session runs as a k3s pod in `vogt-sessions`. Builds happen in the session pod, never in the engine container. A session survives an engine restart or eviction with its workspace, its credentials (including the graft session token) and its conversation. Host placement becomes an explicit, capped exception. Oversight, interactive and shell pods (WI-3170, WI-3171) are phase 2: sequenced here, not detailed.

**Sources, all read-only.**
- Code: vogt git `rust-core` @ `e9a9cc19`. Its trailer `Graft-Changeset: ce761a7bb056…, Graft-Batch: 194` makes it exactly the graft line `rust-2` head that the dev build runs. Clone: `/home/sprooty/Working/Active/.agent-tmp/WI-4426/src`. Paths below are relative to `engine/`.
- Live: `vogt-dev-engine` Deployment revision 95, pod `-pwrds` (restarted 22:47Z after the eviction); the `vogt-sessions` namespace; engine and session-agent logs; `vogt-dev-engine-config`. Copies (redacted) are in `/home/sprooty/Working/Active/.agent-tmp/WI-4426/`.
- Vogt: WI-4426 and every item it lists, with comments. Also WI-3370, WI-3374, WI-3354 and WI-4267..4270.
- Ops smoke harness: `/home/sprooty/Working/Active/.agent-tmp/ops-wsroot/scripts/smoke-pod`.

No code, k8s object or work item was changed. One early-warning note was posted on WI-4426 (§1.4).

> Note on where this file lives: `~/Working/notes` is inside the engine's `agent-config` emptyDir, which a restart wipes. A copy is kept at `/home/sprooty/Working/Active/.agent-tmp/WI-4426/vogt-pod-completion-2026-11-19.md` (workspace PVC).

---

## 0. Summary

Most of the pod backend is built and tested. The bootstrap, bundle, mint and revoke, write-back, hibernate and wake, adoption after restart, the salvage archive, the per-template policy, kache, and Codex/opencode pods are all in the code. **Six concrete defects and two config gaps** keep it from being usable. Each is small, and none needs a redesign:

| # | Blocker | Root cause | Size |
|---|---|---|---|
| B1 | WI-4418: busy pods are killed with "heartbeat silent" | The session agent rebuilds its 1 s `sleep` on every `select!` pass, so a steady stream of output, input or acks starves the heartbeat (and the bundle flush). The engine counts only `Heartbeat` frames as liveness. | S |
| B2 | No graft token in pods ("the project names no graft source") | Project `vogt` has `landing.source = null`. Setting it is blocked by B3. | config, XS |
| B3 | WI-4417: every vogt session start fails | `brief.rs` `LandingFields` is `deny_unknown_fields` and rejects `initiative_bases`, `long_lived_branches` and `source`. | XS |
| B4 | Pod image cannot run clippy or rustfmt | `Dockerfile.pod:279` installs the toolchain with `--profile minimal`, and `RUSTUP_HOME` is read-only in a pod, so a `rust-toolchain.toml` cannot install components. | S |
| B5 | Sessions cannot be woken after a restart ("account at capacity: cap 8") | Recovery restores an account lease for **every** recovered session. One that stays hibernated holds its slot forever, and dev has one account. | S |
| B6 | The engine container fills its 8Gi ephemeral quota | Host sessions write `/tmp`, `/cache` and auto-installed toolchains into the container's writable layer. There is no cap on host sessions, and agent tasks and assistant starts are host-only. | S code + S ops |
| C1 | Rust worker pods are undersized and cold | One global 4 CPU / 6Gi / 20Gi policy; per-pod emptyDir `CARGO_HOME`; kache is read-only. | ops config |
| C2 | Workers still get placed on the host | Oversight and the handover use `placement=host` because pods were broken. Nothing refuses or caps it. | in B6 |

After B1–B6 and C1, phase 1 ends with the live acceptance test: five concurrent Rust worker pods run red/green/clippy/nextest and land; then the engine pod is deleted, gracefully and then forced, and all five resume (§5, item P1.12).

---

## 1. Live state (vogt-dev, 2026-11-19 22:47–23:10Z)

### 1.1 Engine Deployment `vogt-dev-engine` (revision 95)
- Container `engine`: limits cpu 4, memory 24Gi, **ephemeral-storage 8Gi**; requests 500m/6Gi/1Gi. Pinned to `winrarhost4`.
- `VOGT_ENGINE_SESSION_BACKEND=pod`, `ENGINE_POD_BACKENDS_ENABLED=true`, `ENGINE_POD_NAMESPACE=vogt-sessions`, `ENGINE_POD_IMAGE=vogt-dev-pod@sha256:4be1b2f6…`.
- Resources: `ENGINE_POD_RESOURCE_REQUESTS=cpu=250m,memory=512Mi,ephemeral-storage=2Gi` and `…_LIMITS=cpu=4,memory=6Gi,ephemeral-storage=20Gi`. No `ENGINE_POD_TEMPLATE_POLICIES`.
- Nodes: `ENGINE_POD_PREFERRED_NODES=winrarhost4:100,winrarhosttrig:50` and `…_EXCLUDED_NODES=winrarhost3,winrarhost,winrarhost1`.
- Caches: `ENGINE_POD_CACHE_PVC=""`, `…_CACHE_NODE=""`, `…_CACHE_ENV_CONFIG_MAP=session-cache-env`, `…_CACHE_SECRET=kache-s3`, `…_KACHE_CONFIG_MAP=kache-config`.
- Graft: `ENGINE_SOURCE_GRAFT_MINT=true`, the mint token file mounted, `ENGINE_POD_GRAFT_LINE=rust-2`. The log warns that four `ENGINE_POD_GRAFT_*`/`ENGINE_GRAFT_MINT_*` names are deprecated aliases of `ENGINE_SOURCE_GRAFT_*`.
- Accounts: `ENGINE_AGENT_CLAUDE_ACCOUNTS=acct2=…` (one account). The cap is the default 8 (`claude_config.rs:320-324`).
- Volumes:
  - `/home/sprooty` is the `agent-config` **emptyDir, sizeLimit 4Gi**;
  - `~/.claude`, `~/.local/share/vogt-engine` (the state_dir) and `/var/lib/vogt` are the `vogt-dev-state` PVC;
  - `~/Working/Active` is the `vogt-dev-workspace` PVC;
  - `/var/lib/vogt/builds` is the `vogt-dev-builds` PVC (`CARGO_TARGET_DIR`).
  - Nothing is mounted at **`/tmp`, `/cache` or `/opt/rust`**. Those paths are the container's writable layer.
- `engine.toml` sets only `workspace_root = "/home/sprooty/Working/Active"`. There is no `session_templates` override, so the built-in templates are `backend=pod` (`config.rs:150-162`).

### 1.2 `vogt-sessions` namespace
- ResourceQuota (in use at the time of reading / hard limit):
  - pods 2/12
  - requests: cpu 500m/12, memory 1Gi/24Gi, ephemeral 4Gi/80Gi
  - limits: cpu 8/32, memory 12Gi/48Gi, ephemeral 40Gi/160Gi
  - PVCs 2/16
- LimitRange: max per container 8 CPU / 16Gi / 40Gi.
- NetworkPolicies: `default-deny` and `session-egress`. ConfigMaps: `session-cache-env` and `kache-config`. Secret: `kache-s3`.
- PVCs `session-cache-winrarhost1` and `session-cache-winrarhosttrig` (100Gi each) are **unused**: `CACHE_PVC` is empty, and winrarhost1 is an excluded node.
- A live session pod (`vogt-session-20a5343b…`, project vogt) shows:
  - every volume is an emptyDir with no sizeLimit (`session-workspace`, `-cache`, `-home`, `-tmp`, `-work`, `-run`), plus the bootstrap Secret;
  - restricted PSS, `readOnlyRootFilesystem`;
  - `restartPolicy: Never`, `activeDeadlineSeconds: 43200`;
  - `VOGT_SOURCE_JSON = graft vogt rust-2, landing line`.

### 1.3 What the logs show
- **The eviction:** the old engine pod `-5x6xv` has status `Evicted: Container engine exceeded its local ephemeral storage limit "8Gi"`.
- **Recovery after the restart:** the engine "recovered a session as hibernated" for 12 sessions.
  - It woke 2 that were pinned awake: `ad039e34` (cve) and `20a5343b` (vogt-dev), both as **pods**.
  - It refused the rest: `boot wake refused … account at capacity: cap 8; counts {"acct2": 8}` (B5).
- **No graft token:** every new pod logged `WARN session graft token unavailable … reason="the project names no graft source"`. The entrypoint then logged `skipping clone of vogt (line rust-2): no graft token is available`, and the pod runs with no source (B2).
- **The heartbeat kill again:** at 23:02:14Z, `ending a session whose pod died session=ad039e34 … note=agent heartbeat silent for more than 60s`, 14.5 min after start. The agent log of `20a5343b` shows `input from the engine` frames about every 16 ms (B1).
- **Host sessions:** the host sessions restarted by hand at 22:49–22:59 (`launcher="agent-auth"`) include this one and `codex-43-wheel-popup-fix-host`. WI-4417's worker (`ses_01M7TY7M…`) is **blocked**: its oversight-minted graft token at `~/.config/graft/sessions/<id>/trial-1.token` was on the `agent-config` emptyDir and the restart wiped it.

### 1.4 The engine container is filling again (measured 23:06Z, about 19 min after the restart)
Inside the engine container, before any new eviction:

| Path | Usage | Note |
|---|---|---|
| `/tmp` | 3.8G | `.tmp*/`, `chatwidget-tests-*`, `codex-home`: test temp dirs from the host Codex nextest run |
| `/opt/rust/rustup` | 1.4G | a `1.95.0` toolchain auto-installed next to the image's `1.94.0` |
| `/cache` | 0.5G | XDG/npm/pip/uv/cargo-bin caches from the pod-image ENV, which the engine shares (`Dockerfile.pod:75-79`, `269-270`) |

That is about 5.7G of the 8Gi already. An early warning is posted on WI-4426 (`cmt_01M7V0ZBQWBAEDQ7DC5YWEJ6MS`). The cause is structural: the engine image *is* the pod image (`rust2-images.yml:198-199`), so its ENV points every cache into the writable layer.

---

## 2. Gap matrix

Status: **done** = code and evidence meet the item; **partial** = code done, live proof or config missing; **missing**. A ★ marks a close candidate.

| Item | Title (short) | Status | Evidence | What is left |
|---|---|---|---|---|
| WI-3157 | pod backend create/delete | done (closed) | `pod_backend.rs` | none |
| **WI-4418** | heartbeat kill | **missing (bug open)** | sender `session-agent/src/lib.rs:2345-2431` rebuilds `sleep(BUNDLE_DEBOUNCE)` (`:2419`) on each `select!` pass; heartbeat sent only at `:2430`; engine counts only `Frame::Heartbeat` (`server/src/agent_stream.rs:1524-1527`; `Output` at `:1456-1480` does not); kill at `server/src/pod_reconcile.rs:94-104`, default 60 s (`pod_backend.rs:516-517`, env `ENGINE_POD_HEARTBEAT_TIMEOUT` `:771`); live: ad039e34 killed 23:02:14Z | P1.3 |
| **WI-4417** | brief.rs LandingFields | **missing** | `core/src/application/brief.rs:193-207` `deny_unknown_fields`; `LandingPolicy` serialises 3 extra fields (`core/src/application/models/entities.rs:442-466`); error propagates (`brief.rs:163-168`, `:532`) | P1.1 |
| WI-3229 | pod boot follow-ups (graft mint) | partial ★ | item 1 done: per-session mint (`sessions.rs:839-850`, `graft_mint.rs`, scopes `:12-16`, TTL `:45-49,226-232`), revoke on teardown (`graft_provider.rs:437-460`); live mint enabled. Blocked only by `landing.source=null` (live log). Items 2–6: smoke step pushed to ops (not run), fixture, docs, audit ignore | close after P1.2 shows a minted token in a pod; move 2–6 into P1.11 or drop |
| WI-3163 | pod boot entrypoint | done-in-code ★ | `deploy/session-entrypoint.sh` steps validate→handoff (`:13-378`), no TODOs; verified on dev (comment 2026-11-06: warm boot about 17 s, brief seeded). `g status` clean not checked | close; `g status` check folds into P1.12 |
| WI-3161 | MCP and agent-auth in pod | partial ★ | engine renders MCP config into bootstrap (`agent_stream.rs:1182-1210`); entrypoint writes it (`session-entrypoint.sh:92-201`); `mcp-bootstrap.sh` is host-only by design (`Dockerfile:667`). In-pod `vogt-agent-auth fetch`, cadastre lookup and `AGENT_AUTH_MODE=brokered` were never observed | close; fold the three probes into P1.12 |
| WI-3244 | write-back auth | **done in code** ★ | `presenting_session` falls back to `agent_auth.session_for_token` (`secret_broker.rs:1995-2006`, `2026-2028`, added by WI-3334, which is done); test `a_claude_pod_gets_a_signed_in_bundle_and_can_write_it_back` | close as fixed by WI-3334; the live 2xx is observed in P1.12 |
| WI-3160 | credential and transcript write-back | partial | route `secret_broker.rs:112`, `app.rs:578-581`; agent flush on 1 s debounce plus kill/exit/`Flush` (`lib.rs:39`, `2385`, `2400`, `2425`); final-flush tests landed (ca6637fe556f9). **Starved by WI-4418** (same timer). Not observed live | P1.3 fixes the starvation; P1.12 observes a 2xx and a refreshed credential |
| WI-3158 | pod hibernate/wake | partial | hibernate flush→salvage→delete (`sessions.rs:3092-3210`); wake re-creates the pod and resumes the transcript (`hibernation.rs:61-65,132-158`; `lib.rs:1549-1550,1767-1769`); test `integration.rs:23481`. Live: a pod wake was shown; Claude conversation continuation was not shown | P1.12 adds one hibernate/wake round trip; the full item closes there |
| WI-3370 | keep unlanded work (chain WI-4267..4270) | missing | salvage is archive-only (`pod_salvage.rs:12-13`); a lost or killed pod ends the session and removes the record (`pod_reconcile.rs:357-365`, `sessions.rs:2955-2956`); design and decisions D1–D7 done 2026-11-15 | phase 2 (P2.1), see Q6 |
| WI-3162 | session images base/rust/mobile | partial | stages exist (`Dockerfile.pod:49,257,303`; `.github/session-images-bake.hcl:6-29`). **Missing:** clippy, rustfmt and rust-src (`--profile minimal`, `Dockerfile.pod:279`); a `stable` toolchain alias; a project→image map (the engine uses template `VOGT_SESSION_IMAGE` else `ENGINE_POD_IMAGE`, `pod_backend.rs:1481-1485`). The dev image is built with no `target:` so it is the **mobile** stage (`rust2-images.yml:129`); the comment at `Dockerfile.pod:2` is wrong | P1.4 (toolchain), P2.3 (project map) |
| WI-3164 | build caches | partial | kache wired: envFrom `session-cache-env` and `kache-s3`, `/etc/kache` (`pod_backend.rs:1547-1563,1654-1668`); ConfigMap sets `RUSTC_WRAPPER=kache`, shared CI bucket **read-only**; per-session write keys exist (`session_cache.rs:1-45`) but no provisioner URL on dev. Per-pod emptyDir `CARGO_HOME` (`pod_backend.rs:1065-1074`); the `/cache-ro` seed is off; hostPath is refused by PSS (`:81-92`). No timings | P1.6 (measure, size); P2.2 (registry seed, write keys) |
| WI-3354 | per-node caches chosen at schedule time | missing (empty body) | none | P2.2 |
| WI-3166 | per-template resources and affinity | done in code ★(narrow) | `ENGINE_POD_TEMPLATE_POLICIES` JSON: requests, limits, nodeSelector, affinity, deadline (`pod_backend.rs:28-48,754-757,1544,1747-1756`); not set on dev; no measurements; no OOM test | narrow to "set measured defaults on dev and OOM test" = P1.6 |
| WI-3181 | pod spec part 2 (cache/node/ephemeral) | done in code ★ | whole-claim `/cache` and envFrom observed live (comment 2026-11-06); node pin via `CACHE_NODE` → `nodeName` (`pod_backend.rs:1787`); the item's per-node writable cache premise is superseded by WI-3242 (`pod_backend.rs:1039-1044,1585-1613`) | close; the remainder is P1.6 config |
| WI-3169 | workers and reviews as pods on dev | partial | 5-pod runs on 2026-11-06 (README and review tasks only, nothing landed, no Rust gates); trust prompt and task seeding fixed (WI-3355, WI-3356 done) | **P1.12 acceptance closes it** |
| WI-3159 | account bundles and caps | partial (deferred) | pool and cap (`account_pool.rs`; `claude_config.rs:93-131,320-324`); one cap for every account; operator deferred three accounts until WI-3152 (2026-11-15). **New bug B5** in lease recovery (`sessions.rs:4750`, `account_pool.rs:80-89`) | P1.5 (bug); phase 2 for more accounts |
| WI-3221 | smoke-pod harness | done ★ | in ops `scripts/smoke-pod`: lost pod → FAIL (`:207-221,303,577,584-590`); diagnostics (`:260-293,568-573`); unique tag (`:107-127,164`); network policy allows sessions→engine (`apps/vogt-smoke/networkpolicy.yaml:29-37,85-89`), entrypoint retries (`session-entrypoint.sh:39-48`). Item 5 (leftover `--keep` objects) not checked | close; open a new chore if leftovers remain |
| WI-895 | per-session cgroups | missing on rust-2 | branch `wi-895-session-cgroups` (90f7a48) was never ported; `resources.rs:3-4` is visibility only. With workers in pods, only the capped host exceptions need it | ★ close as superseded by pods + P1.8, or keep p3 for host-only |
| WI-2698 | prod: keep compile jobs off the GUI pod | partial (prod) | option (a) nice/CARGO_BUILD_JOBS shipped on prod; 24 h window blocked on scraping | ★ supersede by "prod sessions as pods" after phase 1; keep p3 until then |
| WI-3170 | oversight and interactive pods | missing | no role-specific pod launch; `placement` ignores the role (`sessions.rs:4065-4077`) | phase 2 (P2.5) |
| WI-3171 | shells and agent tasks as pods; delete local PTY | missing | agent tasks are `template: None, placement: None` → host (`agent_tasks.rs:1469-1470`); assistant `placement: None` (`assistant.rs:3292,4622`); `pty.rs` present | phase 2 (P2.6) |

Two smaller gaps the matrix exposes:
- Long-lived pods past the 12 h graft TTL lose `g land`: there is no re-mint for a live pod (`graft_mint.rs:45-49`). This becomes P2.4.
- `bundle_snapshot` scans the account's whole `projects/` tree at each pod start and silently drops transcripts over 16 MiB (`secret_broker.rs:144,1331-1370`). This becomes P2.7.

---

## 3. Root causes and sizing of the current blockers

### B1: WI-4418 heartbeat kill (S, Opus)
- **Sender.** `session-agent/src/lib.rs` runs `loop { tokio::select! { output_rx.recv(), stream.next(), _ = tokio::time::sleep(BUNDLE_DEBOUNCE) => { flush_bundle; check CLI exit; send Heartbeat } } }`. The `sleep` is created fresh on every pass. Any frame resets it: PTY output, the engine's `Ack` for each output chunk, and `Input` (live, the engine sends input frames about every 16 ms). So while a session is busy the timer never fires.
- **Checker.** The engine's liveness is `link.note_heartbeat()`, called only for `Frame::Heartbeat` (`agent_stream.rs:1524-1527`). `pod_reconcile` ends the session after 60 s of silence.
- **Why it isn't always about 60 s.** The kill comes after the first busy minute with no 1 s gap. That was 60 s in WI-4418's run and 14.5 min for ad039e34.
- **Side effects of the same starvation.**
  - The bundle flush stops, so transcript and credential write-back stop (WI-3160).
  - CLI-exit detection stops.
  - On the kill, `end_session` deletes the pod **without salvage** and the record is removed (`pod_reconcile.rs:157-161`, `sessions.rs:878-901`, `2955-2956`). The work is lost.
- **Fix, two halves.**
  - Agent: one `tokio::time::interval(BUNDLE_DEBOUNCE)` with `MissedTickBehavior::Delay`, created before the loop.
  - Engine: any accepted agent frame (`Output`, `Gap`, `Flushed`, `Heartbeat`) refreshes liveness. The engine half protects pods still running an old image.
- **Tests (fail without the fix).** Under paused tokio time, output every 300 ms for 2 min: the session stays alive, at least 100 heartbeats are sent, and the flush runs. A silent agent is still ended after 60 s.

### B2 + B3: no graft token in pods (XS code + XS config)
- The pod mint is gated on the **project's** declared source. `SourceProvider` refuses with "the project names no graft source" when `landing.source` is null (live log; WI-3229 hand-off of 2026-11-13). WI-4213 (`2fbef5eb`) let a landing policy state its source.
- Project `vogt` on dev has `"source": null` (project_get). It cannot be set usefully today, because the engine's `brief.rs` rejects any landing rule carrying `source` or the other two fields (B3, WI-4417). The project rule text still tells workers to read an oversight-minted token from `~/.config/graft/sessions/…`, which lives on the engine emptyDir and dies with every engine restart (live: WI-4417's own worker is blocked on exactly that).
- **Fix.** WI-4417 (3 lines and a test). Then `project_update` vogt with `landing.source = {kind: graft, endpoint: graft-dev, repo: vogt, line: rust-2}`, and the same for every graft-sourced project on dev (Q8). In a pod, the entrypoint already writes the minted token to `~/.config/graft/sessions/<id>/trial-1.token` (`session-entrypoint.sh:221-260`), so the project rule's path stays valid in pods. Only the sentence "minted by oversight" changes.
- **Survival.** Engine restart: the pod keeps its token on its HOME emptyDir, and the engine keeps a copy at `<state_dir>/pod-graft-tokens/<id>` on the PVC for revoke. Wake: a new pod, a new mint. Gap: no re-mint past 12 h (P2.4).

### B4: pod image toolchain (S, Sonnet)
- `Dockerfile.pod:279`: `rustup-init … --default-toolchain ${RUST_VERSION} --profile minimal`, then targets and `rust-analyzer` only. There is no clippy, rustfmt or rust-src.
- In a pod `RUSTUP_HOME=/opt/rust/rustup` is read-only (`pod_backend.rs:1760`). `rustup component add` fails, and so does a `rust-toolchain.toml` naming `stable` (graft's does). The image's toolchain is named `1.94.0`.
- On the engine the same path is *writable* (it is the container layer). That is why a 1.95.0 toolchain, about 1.4G, appeared there (§1.4).
- The dev image is built without `target:`, so it is the **mobile** stage (about 4 GB). That works for vogt, but it is the wrong layer and slows a cold pull.
- **Fix.**
  - Install the `default` profile with `rust-src`.
  - Pre-install the pinned toolchain under the `stable` name too (`rustup toolchain link stable …`, or install `stable` at the same pinned version), so toolchain files resolve read-only.
  - Set `RUSTUP_AUTO_INSTALL=0` in the image ENV, so a mismatch fails loudly instead of writing.
  - Build the `rust` target for vogt-dev.
  - Correct the stale comments (`Dockerfile.pod:2`, `pod-base.yml:6-9`, `Dockerfile:34-37`).
  - Add an image test: `cargo clippy --version && cargo fmt --version && cargo nextest --version` as the vogt user with a read-only `/opt/rust`.

### B5: account leases leak across restart (S, Sonnet)
- `recover_records` (`sessions.rs:4731-4757`) calls `account_pool.restore(record.account)` for every recovered record and keeps the lease in `account_leases`. `restore` ignores the cap (`account_pool.rs:80-89`).
- A wake reuses that lease (`sessions.rs:1690-1722`). A recovered session that is **not** woken (only `keep_awake` ones wake at boot) keeps its slot until someone wakes or removes it.
- Sessions that were already hibernated before the restart have no restored lease, so waking them needs a fresh one, and that fails with 429 while the recovered-but-asleep sessions hold every slot. Live: 12 recovered, cap 8, only the 2 pinned ones woke.
- **Fix.** A recovered session holds no slot while hibernated; it takes one at wake. Boot wakes the pinned sessions first, then lets the rest queue. Test: 12 recovered on cap 8, wake 3 → all succeed, and the counts reflect only live sessions.
- **Config:** the cap for a single account (Q3).

### B6: engine ephemeral storage and the host-placement cap (S code, S ops)
- **What writes into the 8Gi.** Host PTY sessions inherit the engine env (`pty.rs:1429-1477`), which comes from the pod image:
  - `/tmp`, including test temp dirs and `ENGINE_SHARED_CARGO_TARGET_DIR`'s default `/tmp/claude-1000/cargo-target` (`config.rs:1642-1659`, `session_scratch.rs:30-42`) when `CARGO_TARGET_DIR` is not set;
  - `/cache/{xdg,npm,pnpm,pip,uv}` and `/cache/cargo/bin`;
  - rustup auto-installs into `/opt/rust/rustup`.
  - The engine never reads `CLAUDE_CODE_TMPDIR`, so scratch prune misses `.agent-tmp`.
  - The free-space check reads node disk free, not the container quota (`sessions.rs:2087-2090`, `session_scratch.rs:355-366`).
- **Why there were 12 host sessions.**
  - `runs_in_pod` (`sessions.rs:4065-4077`) honours `placement=host` with no limit.
  - A missing or unresolvable template silently means host.
  - Agent tasks and the assistant always pass `placement: None, template: None` → host (`agent_tasks.rs:1469-1470`; `assistant.rs:3292,4622`).
  - There is no cap anywhere.
- **Fix: code.**
  - (a) A host-placement policy in `runs_in_pod` / `validate_template_placement` (`sessions.rs:101-150`, `2371-2387`):
    - worker and review roles are refused host unless `ENGINE_HOST_PLACEMENT_ALLOW_WORKERS=1`;
    - a missing or unresolvable template is an error, not host;
    - a concurrent host-session cap, `ENGINE_HOST_SESSION_CAP` (refusal 429 with counts);
    - agent tasks and the assistant count against the cap.
  - (b) Each host session gets `TMPDIR`, `CARGO_TARGET_DIR` and `XDG_CACHE_HOME` under a per-session dir on a PVC, set in `pty.rs:1431-1477`.
  - (c) The free-space check also reads the container's own ephemeral usage.
- **Fix: ops (phase 0, no code).**
  - Mount `/tmp` and `/cache` from the `vogt-dev-builds` PVC (subPaths `engine-tmp`, `engine-cache`).
  - Set `RUSTUP_AUTO_INSTALL=0` on the engine.
  - Point `ENGINE_SHARED_CARGO_TARGET_DIR` at the builds PVC.
  - Optionally raise the engine ephemeral limit while host sessions remain (Q1).

### C1: Rust worker pod sizing and caches (ops config, Sonnet to measure)
- The current global limits are 4 CPU / 6Gi / 20Gi ephemeral.
- `/work/target` (no `CARGO_TARGET_DIR` set in pods) plus kache's local cache (`local_max_size=10GiB`) plus a cold registry all share that 20Gi. An engine-sized Rust build at 6Gi is borderline (inference; measure in P1.6).
- Five pods at 4 CPU each on winrarhost4 (16 cores) contend, so spill-over to winrarhosttrig is expected.
- The quota fits five pods at 8Gi/30Gi: limits.memory 40/48Gi, limits.ephemeral 150/160Gi. That is tight with other pods; raise the quota or lower ephemeral to 25Gi (Q4).

---

## 4. Target design (phase 1 end state)

1. **Placement.**
   - Every session with role worker or review runs as a pod; host is refused for those roles.
   - Host is allowed for oversight, interactive, shell, agent tasks and the assistant, under `ENGINE_HOST_SESSION_CAP` (dev: Q2).
   - Each host session gets its own PVC-backed `TMPDIR`, `CARGO_TARGET_DIR` and `XDG_CACHE_HOME`. The engine's `/tmp` and `/cache` are on the builds PVC, so a host session can no longer evict the engine.
   - Unresolved templates are an error.
2. **Builds.** In the session pod only.
   - Image: the `rust` stage with clippy, rustfmt, rust-src and nextest, and a read-only toolchain that resolves `stable`.
   - Sizing: a per-template policy for worker templates (`ENGINE_POD_TEMPLATE_POLICIES`).
   - Caches: kache read-only on the shared CI bucket, plus a per-pod registry (cold; seeded in phase 2).
3. **Credentials.**
   - The graft token is minted per pod from the project's `landing.source`, delivered once at bootstrap, written 0600 on the pod HOME, copied engine-side to `<state_dir>/pod-graft-tokens` for revoke, and revoked on teardown.
   - The Claude account bundle is seeded at bootstrap; refreshed credentials and transcripts go back through the bundle-file route on a working 1 s timer.
4. **Engine restart or eviction.**
   - Pods are not owned by the engine process. Records and pod stream tokens are on the state PVC from create time (`sessions.rs:~2520-2578`, `4833-4836`).
   - At boot `readopt_pods` (`sessions.rs:1054-1144`, run before serving, `app.rs:804`) re-attaches each pod at its log cursor. The agents reconnect with backoff (`lib.rs:2489-2512`).
   - The workspace (`/work`), the conversation (in the pod and written back) and the graft token (on the pod) never leave the pod, so nothing is lost.
   - A graceful shutdown leaves pods alone (`sessions.rs:3868-3875`). A forced kill must give the same result, and P1.12 tests both.
5. **Session-pod loss** (eviction, node loss, OOM) is **phase 2** (WI-3370 chain): a periodic graft-draft save and a `pod_lost` hibernation.

---

## 5. Plan

Conventions:
- Each item is one area, lands on graft `vogt/rust-2` with `g land`, and has at most 5 numbered done-when lines.
- `files` names the edit scope; `deps` must have landed (or, for ops, merged and synced) first.
- Model: Opus for concurrency, auth or credential paths; Sonnet otherwise; Haiku for docs/config.
- Engine items that touch `sessions.rs` (P1.5, P1.8) must not run concurrently.

### Phase 0: stop the bleeding (ops only, today)

**P0.1 Engine storage off the writable layer** (ops `apps/vogt-dev/deployment.yaml`; Haiku/Sonnet; deps none; operator approves deploy)
1. `/tmp` and `/cache` in the engine container are subPath mounts of `vogt-dev-builds` (`engine-tmp` with mode 1777 via the existing init container; `engine-cache`).
2. Engine env `RUSTUP_AUTO_INSTALL=0`, and `ENGINE_SHARED_CARGO_TARGET_DIR` under `/var/lib/vogt/builds`.
3. Ephemeral limit per Q1.
4. After the rollout, `du -sxh /` of the writable layer (excluding mounts) is under 1G after 1 h of normal host load.

### Phase 1: workers and reviews as pods; survive an engine restart

**P1.1 WI-4417: LandingFields accepts the full LandingPolicy** (`core/src/application/brief.rs`; Sonnet low; deps none). The item's own done-when stands. Note: its existing worker is blocked on a lost token. Restart it after P1.2 runs in a pod, or have oversight re-mint for it.

**P1.2 Declare graft sources on dev projects** (data/config via `project_update`, plus the project rule text; Haiku; deps P1.1 deployed)
1. `vogt` (and every graft-sourced dev project per Q8) has `landing.source = {graft, graft-dev endpoint, repo, line}`.
2. The `vogt` project rule says "the token is minted by the engine for this session" (path unchanged).
3. A new vogt pod logs no `graft token unavailable`, and its entrypoint logs `step clone` with no WARNING; `g status` inside is clean on `rust-2`.
4. WI-3229 is closed with that evidence.

**P1.3 WI-4418: heartbeat survives sustained traffic** (`session-agent/src/lib.rs`, `server/src/agent_stream.rs`, tests in both; Opus medium; deps none)
1. The agent's flush/heartbeat timer is one `interval` created before the loop (`MissedTickBehavior::Delay`). Frames on other branches do not delay it.
2. The engine refreshes liveness on every accepted agent frame, not only `Heartbeat`.
3. A paused-time test with output every 300 ms and input every 16 ms for 2 min: no kill, ≥100 heartbeats, ≥100 flush attempts. It fails on the current code.
4. A silent agent is still ended after `heartbeat_timeout` (existing or new test).
5. `cargo clippy` and `nextest` for `vogt-engine-server` and `vogt-session-agent` are green.

**P1.4 WI-3162 (rescoped): rust stage toolchain is complete and read-only-safe** (`engine/Dockerfile.pod`, `.github/workflows/rust2-images.yml`, `pod-base.yml` comments, an image test script; Sonnet; deps none)
1. The `rust` stage installs the pinned toolchain with clippy, rustfmt and rust-src, also reachable as `stable`; `RUSTUP_AUTO_INSTALL=0`.
2. vogt-dev's pod image is built from `target: rust` (the mobile stage stays a separate tag).
3. An image test runs as uid 1000 with `/opt/rust` read-only: `cargo fmt --version`, `cargo clippy --version`, `cargo nextest --version`, and `cargo clippy` in a project whose `rust-toolchain.toml` says `stable`. It fails on the current image.
4. The stale comments are fixed. The image size is recorded on WI-3162.

**P1.5 Account leases: recovered-but-hibernated sessions hold no slot** (new bug item; `server/src/sessions.rs` (recover and wake), `server/src/account_pool.rs`, tests; Sonnet medium; deps none; not concurrent with P1.8)
1. After recovery, `counts()` equals the number of live or pinned-awake sessions per account, not the number of recovered records.
2. Boot wakes pinned sessions first. Any hibernated session can then be woken while live count < cap.
3. Test: 12 recovered records on cap 8 with 2 pinned → boot leaves counts 2; 6 further wakes succeed; the 7th is 429. It fails on the current code.
4. The existing account_pool tests stay green.

**P1.6 Rust worker pod sizing, from measurement** (ops `apps/vogt-dev/deployment.yaml` (`ENGINE_POD_TEMPLATE_POLICIES`), `apps/vogt-sessions` quota; Sonnet; deps P1.4 deployed; rescopes WI-3166, absorbs WI-3181's remainder)
1. Measure one vogt worker pod running `cargo clippy --all-targets` plus `cargo nextest run -p vogt-engine-server`, cold and with kache read-only: peak memory, peak ephemeral (`/work/target`, `/cache`), wall time. Record on WI-3166 and WI-3164.
2. A template policy for the worker and review templates sets requests and limits from the measurement (Q4 default: limits 4 CPU / 8Gi / 30Gi).
3. The quota admits 5 such pods plus 3 others.
4. A deliberate over-limit build in a pod is OOM-killed in that pod only, and the session shows the reason.

**P1.7 Engine restart with live pods: ungraceful path and test hardening** (`server/src/sessions.rs` (readopt) or `pod_reconcile.rs` only if a defect is found, `server/tests/integration.rs`; Opus medium; deps P1.3)
1. An integration test kills the engine with no shutdown hook while a pod session is mid-output. A new engine adopts it, and output after the restart is continuous at the cursor with no duplicates.
2. The same with a graceful shutdown (existing test `integration.rs:23735/23822`, kept green).
3. The load-only race `a_restarted_engine_adopts_the_running_pod_and_replays_exactly` (reported on WI-3229) is fixed or shown gone in 20 consecutive full-suite runs.
4. An adopted link gets a full `heartbeat_timeout` from its reconnect before liveness is judged.

**P1.8 Host placement is an explicit, capped exception** (new item; `server/src/sessions.rs` (`runs_in_pod`, `validate_template_placement`, spawn), `server/src/pty.rs` (env), `server/src/config.rs`, `server/src/agent_tasks.rs` and `assistant.rs` (pass role and template only), docs ENGINE.md §placement; Opus medium; deps P1.5 landed (same file))
1. Worker and review sessions are refused `placement=host` (409 with the reason) unless `ENGINE_HOST_PLACEMENT_ALLOW_WORKERS=1`. A missing or unresolvable template is a 400, never a silent host.
2. `ENGINE_HOST_SESSION_CAP` (default 0 = unlimited; dev per Q2) caps concurrent host sessions including agent tasks and assistant starts, with a 429 carrying the counts.
3. Each host PTY gets `TMPDIR`, `CARGO_TARGET_DIR` and `XDG_CACHE_HOME` under `<builds root>/<session>/`, removed at session end.
4. Tests cover 1–3 and fail on the current code.

**P1.9 Engine free-space check sees its own quota** (`server/src/session_scratch.rs`, `server/src/sessions.rs` read site only; Sonnet low; deps P1.8)
1. The low-space refusal and alert also trip when the engine container's ephemeral usage, read from cgroup or the downward API `ephemeral-storage` limit, passes 75% of its limit.
2. Scratch prune also covers `CLAUDE_CODE_TMPDIR` when set.
3. Tests with a fake limit.

**P1.10 Ops: dev sends workers to pods** (ops `apps/vogt-dev`, oversight worker-setup and the worker-brief skill; Haiku; deps P1.8 deployed)
1. `ENGINE_HOST_SESSION_CAP` and the per-template policy are set on dev.
2. `worker-setup.md` and the worker-brief skill say: pods by default, never `placement=host` for a worker, the token is engine-minted, and `g` works in `/home/vogt/Working/session` (the WI-3169 doc remainder).
3. The deprecated `ENGINE_POD_GRAFT_*`/`ENGINE_GRAFT_MINT_*` env names are renamed to `ENGINE_SOURCE_GRAFT_*` (the log warning is gone).

**P1.11 WI-3229 leftovers and docs** (`docs/ENGINE.md` pod boot, `deploy/tests/session-entrypoint.sh` fixture, CI audit ignore; Haiku; deps P1.2)
1. The ENGINE.md pod-boot, placement and restart sections match §4 of this doc.
2. The stale entrypoint fixture is refreshed and green.
3. The RUSTSEC-2024-0449 ignore is consistent between `deny.toml` and the CI audit step.

**P1.12 Live acceptance on vogt-dev** (oversight-driven chore; no code; Opus for the oversight session; deps P0.1, P1.1–P1.8 and P1.10 deployed; the operator approves the deploy and the restart)
1. Five concurrent vogt worker sessions (pods, the rust image, the worker template), each with a tiny real brief: add a failing test (red), make it pass (green), then `cargo fmt --check`, `cargo clippy -p <crate> --all-targets -D warnings` and `cargo nextest run -p <crate>`, then `g land` on `rust-2` and post the hand-off. All five land. Record per pod: boot time, build times, peak memory and ephemeral, node, and the graft mint/revoke in graft-dev's audit.
2. Mid-run (at least 3 sessions in a cargo build), `kubectl -n vogt-dev delete pod <engine>` (graceful). After the new engine is ready, all 5 sessions show the same ids, their pods are unchanged (same UID, no restart), the screens continue, and each finishes and lands.
3. Repeat step 2 with `--grace-period=0 --force` on a second batch of 5 (or the same 5 before they land); all resume.
4. During the run, observe at least one bundle-file write-back 2xx per pod in the engine log and one refreshed-credential write (closes WI-3244 and WI-3160). In-pod `vogt-agent-auth fetch`, a cadastre `lookup` and `AGENT_AUTH_MODE=brokered` succeed (closes WI-3161).
5. One session is hibernated and woken and continues its conversation (closes WI-3158's live proof). WI-3169 is closed with the numbers.

Dependency graph (phase 1):
```
P0.1 ─────────────────────────────────────────────┐
P1.1 ─(deploy)─ P1.2 ─ P1.11                      │
P1.3 ─ P1.7                                       ├─ P1.12
P1.4 ─(deploy)─ P1.6                              │
P1.5 ─ P1.8 ─ P1.9                                │
            └─(deploy)─ P1.10 ────────────────────┘
```
Parallel start: P0.1, P1.1, P1.3, P1.4 and P1.5 touch disjoint areas. That is two dev deploys before acceptance (after P1.1/P1.3/P1.4/P1.5, then after P1.7/P1.8/P1.9).

### Phase 2: sequence only

- **P2.1 Pod-loss survival** (WI-3370 → WI-4267 A, 4268 B, 4269 C, 4270 D): a periodic graft-draft save; a lost pod becomes a `pod_lost` hibernation; restore on wake. This is the prerequisite for session-pod eviction or node loss being survivable.
- **P2.2 Caches** (WI-3164, WI-3354): a read-only cargo registry seed per node (a cron job fills a PVC mounted at `/cache-ro`), per-session kache write keys via the provisioner, cold/warm numbers. Delete or repurpose the unused `session-cache-winrarhost1/-trig` PVCs.
- **P2.3 Project→image map** (WI-3162 remainder): base for Python-only, rust for vogt and graft, mobile for FarmEggs.
- **P2.4 Graft token re-mint for long-lived pods** (new): re-mint before the 12 h TTL through the bootstrap channel.
- **P2.5 Oversight and interactive pods** (WI-3170; needs P2.1 and WI-3374/3373).
- **P2.6 Shells and agent tasks as pods; delete the local PTY** (WI-3171). Host cap goes to 0, and WI-895 is closed as moot.
- **P2.7 Bundle snapshot cost** (new, small): index the account transcripts instead of scanning `projects/`, and surface the 16 MiB drop.
- **P2.8 Accounts** (WI-3159 after WI-3152): more than one Claude account, per-account caps.
- **P2.9 Prod** (WI-2698): the same shape on vogt-prod once dev passes P1.12.

### Close candidates (to confirm, Q7)
- Close now: WI-3244 (fixed by WI-3334's fallback); WI-3221 (harness items 1–4 fixed in ops); WI-3181 (code done; premise superseded by WI-3242); WI-3163 (verified boot; `g status` moves to P1.2).
- Close at a named step:
  - WI-3229 at P1.2;
  - WI-3161, WI-3160 and WI-3158 at P1.12 steps 4–5;
  - WI-3169 at P1.12.
- Narrow: WI-3166 → P1.6; WI-3162 → P1.4 + P2.3.
- Supersede: WI-895 → P1.8 + P2.6; WI-2698 → P2.9.

---

## 6. Operator decisions

- **Q1. Engine storage now (P0.1).**
  - (a, recommended) Move `/tmp` and `/cache` onto the builds PVC, set `RUSTUP_AUTO_INSTALL=0`, and raise the engine ephemeral limit to 16Gi until P1.8 lands.
  - (b) Only raise the limit.
  - (c) Pause host workers until phase 1. At 23:06Z the container held about 5.7G of 8Gi, so this is urgent.
- **Q2. Who may run on the host, and the cap on dev (P1.8).**
  - (a, recommended) Oversight, interactive, shell, agent tasks and the assistant only; worker and review refused; `ENGINE_HOST_SESSION_CAP=4`.
  - (b) The same with cap 8.
  - (c) Allow workers on the host behind the override flag during the transition.
- **Q3. Account capacity with only acct2 (B5).**
  - (a, recommended) Raise `ENGINE_AGENT_CLAUDE_ACCOUNT_CAP` to 12 on dev for phase 1 (5 workers, 2–3 reviewers, oversight and interactive) and ship P1.5.
  - (b) Keep 8 and run acceptance with fewer other sessions.
  - (c) Bring a second account forward (WI-3159, despite the WI-3152 deferral).
- **Q4. Rust worker pod size (P1.6).**
  - (a, recommended) Limits 4 CPU / 8Gi / 30Gi, requests 1 CPU / 2Gi / 10Gi; raise the `vogt-sessions` quota to limits.memory 64Gi and limits.ephemeral 240Gi.
  - (b) Keep 6Gi/20Gi and the quota as is.
  - (c) Decide after the P1.6 measurement.
- **Q5. Caches in phase 1.**
  - (a, recommended) Cold registry per pod plus read-only kache; measure; seed in P2.2.
  - (b) Enable per-session kache write keys (the provisioner) in phase 1.
  - (c) Bring the registry seed PVC into phase 1.
- **Q6. Session-pod loss (WI-3370 chain).**
  - (a, recommended) Phase 2 right after P1.12. Phase 1 makes the *engine* restart survivable; a session pod's own eviction still loses unlanded work.
  - (b) Pull WI-4267/4268 into phase 1 and add "delete one session pod, it resumes" to P1.12.
- **Q7. Close and narrow** the items listed in §5 "Close candidates".
  - (a, recommended) Yes, as listed.
  - (b) Only close the "close now" group; keep the rest open until P1.12.
- **Q8. Graft sources on dev projects (P1.2).**
  - (a, recommended) Set `landing.source` for every project that lands through graft on dev (vogt, graft, nzbvfs, codex, …) once P1.1 is deployed.
  - (b) vogt only for phase 1.
- **Q9. Deploy windows.** Phase 1 needs two dev engine and pod-image deploys and one engine restart for P1.12.
  - (a, recommended) Deploy as each pair is ready, and run P1.12 in an announced window with no other dev sessions.
  - (b) One combined deploy after all of phase 1 lands.
