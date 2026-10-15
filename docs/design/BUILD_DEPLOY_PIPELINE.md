# Build → deploy pipeline: thin Woodpecker, build once in GitHub

Status: design (WI-1646). Nothing here is implemented. No CI was changed and nothing was deployed or triggered while writing it.
Input: `BUILD_CYCLE_REVIEW.md` (2026-10-15), review items 2, 3, 4 and 6.
Operator decision this design follows: keep Woodpecker, but make it a thin promote/deploy step; build the estate toolchain layers in GitHub; trigger the dev lane automatically from a green `main` `build.yml`.

## 1. Where we are (read from source, 2026-10-15)

| Piece | What it does today | Cost |
|---|---|---|
| `build.yml` `image` (core) | Builds a candidate, smokes it, rebuilds and pushes, then signs. | Starts at 4.7 min and takes 1.4 min. |
| `build.yml` `pod-base` | Looks up `vogt-pod-base:lean-<hash>-<week>` and builds it only when the tag is missing. | Ends at 6.6 min. |
| `build.yml` `stack-image` | Has `needs: [base-images, pod-base, image]`. It runs three BuildKit invocations: the candidate (`load: true`, 303 s), the `codex-gitguard-e2e` target to a local output (128 s), and the final `push: true` (137 s, all cache hits). It then signs. | Starts at 8.0 min and takes 11.0 min. |
| vogt-estate `track-main.yml` | Has five steps: resolve, build, deploy, smoke, receipt. `build` runs `docker build --pull FROM` the new stack digest, so every layer is rebuilt every time: tailscale apt, cadastre pip, theclawbay npm, Flutter, Chrome, uv python 3.12, terraform, sshpass, pingcli, actionlint, komodo-mcp `COPY`, and the bootstrap scripts. | 292 s total, of which `build` is 266 s (#121). Pipelines #109–#121 ran 281–403 s. |
| Dev trigger | Manual. **The `track-main` cron (id 3, `*/10`) and the `demo` cron (id 4) already exist on repo 95 but are `enabled: false`.** | A person has to act. |
| vogt-estate `release.yml` | Resolves, cosign-verifies against `release.yml@refs/tags/v*`, gates on the dev receipt, checks that the estate digest exists, then deploys, smokes and writes the receipt. It builds nothing, except on break-glass. | 41–77 s. |

Two facts constrain the design:

1. **The estate image is not all public.** `komodo-mcp` comes from the private estate registry (`repo.indexarr.net/indexarr/komodo-core-infisical`). `deploy/mcp-bootstrap.sh`, `vogt-cadastre-mcp` and `vogt-egressy-web-headers` are estate files. GitHub runners must not need estate registry credentials. So "fold everything into GitHub" means folding in the network-fetched toolchain. A small private layer that is only `COPY`s stays in Woodpecker.
2. **The only Woodpecker API token on record** is `infisical://infrastructure/prod/WOODPECKER_TOKEN`, and it belongs to `sprooty`, a Woodpecker **admin** (Cadastre note: "agents can create, update and delete org- and repo-level secrets with it"). It must not become the GitHub → Woodpecker trigger credential.

## 2. Target shape

```
GitHub build.yml (main push)
  base-images ─┬─ pod-base(lean) ──┐
               ├─ pod-base(workbench) ─┐
               └─ (bake) core + stack ─┴─► stack-image: push by digest → smoke digest → gitguard on digest
                                          → imagetools tag sha-<c> → cosign sign
                                       └─► stack-workbench-image: same, FROM workbench base, tag sha-<c>
GitHub dev-trigger.yml (workflow_run: build.yml success on main)
  OIDC → Infisical → WOODPECKER_DEV_TRIGGER token → POST /api/repos/95/pipelines {LANE=dev, SOURCE_SHA}
Woodpecker track-main (manual from trigger | cron fallback)
  resolve(SOURCE_SHA) → cosign verify (incl. workbench) → thin estate build (COPY only, cached, ~15–30 s)
  → repin + DeployStack → smoke → receipt
Woodpecker release (unchanged shape): verify release identity → gate on dev receipt → promote estate digest
```

### 2.1 Item 2: estate layers built in GitHub

**Recommendation: a public `workbench` pod-base variant, and a stack image built on it.**

- `engine/Dockerfile.pod` gets a third variant, `workbench`. It is `full` (which already carries Flutter) plus everything in the estate Dockerfile that is fetched from the public internet and is not estate-specific: Google Chrome and the `chrome-ci` wrapper with `CHROME_EXECUTABLE`, uv-installed Python 3.12 in `/opt/uv-python`, terraform, sshpass, pingcli 0.8.4 (keeps the pin note), actionlint, and the tailscale client (client only; nothing joins at build time). These are generic dev-pod tools, so they are not an estate leak. The pins and sha256 checks move over unchanged.
  - `pod-base.yml` already keys tags on the Dockerfile hash plus the ISO week and builds only when the tag is missing. So the workbench base costs one build a week, or one per Dockerfile edit. On every other run it is a lookup costing seconds. This is the "cached in GitHub" part, and it needs no new caching mechanism.
- `build.yml` gets a `stack-workbench-image` job. It runs the same `engine/Dockerfile` with `POD_BASE_IMAGE=<workbench digest>` and publishes to a **separate repository**, `ghcr.io/thedancingdeveloper-org/vogt-stack-workbench`, tagged `sha-<c>`. A separate repository follows the file's own rule that one repository holding two kinds of tag is how someone eventually pins the wrong one. The job is smoked and signed exactly like `stack-image`.
  - Its `server-build`, `web-build` and `core` stages are cache hits from the shared `stack-<ref>` cache ref. Only the runtime stage runs again, which is the agent-CLI install and the COPYs.
  - Estimate: 2–3 min. It runs in parallel with `stack-image`, so it is off the critical path unless the `publish` runner queue serialises the two jobs (see risks).
- **What stays in the estate Dockerfile:** `FROM vogt-stack-workbench@<digest>`, plus three small layers:
  1. `COPY --from=komodo-mcp` (private registry, digest-pinned);
  2. `COPY --from=estate-tools`, where `estate-tools` is a stage `FROM` a **pinned** workbench pod-base digest. It runs `pip install --target /opt/estate/py "cadastre[mcp-client]"` and `npm install -g --prefix /opt/estate/npm theclawbay@<pin>`. Because its `FROM` is pinned rather than the per-commit stack, the stage is a cache hit on every run until a pin changes;
  3. the bootstrap rename plus `COPY deploy/*`.

  The lane drops `--pull` and builds with `--cache-from`/`--cache-to` against `repo.indexarr.net/indexarr/vogt-stack-estate:buildcache`, or simply relies on the Node B agent's local cache, since `proxmox: "true"` pins this lane to one host. Expected build time is 15–30 s, almost all of it the pull of the new stack layers that the deploy pulls anyway.

**Alternative, not recommended for now: zero build in Woodpecker.** Ship the private bits at runtime instead: the bootstrap scripts as ops-repo bind mounts, and `komodo-mcp` and cadastre installed by a pre-start hook. The estate lane would then repin the public `vogt-stack-workbench` digest directly. It is cleaner in the long run, but it moves failure from build time (loud) to pod start (quiet), and it changes the ops overlay as well as CI. It can be revisited after this lands. Recorded as WI step 9 (optional).

**Rejected:** putting the workbench tools in the lean product `vogt-stack`. That would bloat every self-hoster's image by Flutter (~1.5 GB) and Chrome for one estate's needs.

### 2.2 Item 3: build the stack image once

In `stack-image`, and the same in `stack-workbench-image` and the core `image` job:

1. **One build that pushes by digest.** Use `outputs: type=image,name=${STACK_IMAGE},push-by-digest=true,name-canonical=true,push=true` with `provenance: true` and `sbom: true` and no tags. Take the digest from `steps.build.outputs.digest`. `cache-to mode=max` happens once.
2. **Smoke that digest.** `docker pull ${STACK_IMAGE}@${digest}` and run the existing checks against it: both halves start, `vogt-verify-agent-clis`, `claude`/`codex --version`. The pull costs seconds because the runner just built it, and only the layers move.
3. **Gitguard against the same digest.** Build the `codex-gitguard-e2e` target **in the same invocation**, as a second bake target (`docker buildx bake` with `stack` → image and `gitguard` → local). This avoids a third BuildKit pass. Then run it inside the pulled digest exactly as today (`docker create` + `docker cp`).
4. **Tag only on success.** `docker buildx imagetools create -t ${STACK_IMAGE}:sha-<c> ${STACK_IMAGE}@${digest}` is a registry-side manifest write that takes seconds. Then `cosign sign` the digest.

This keeps the invariant "nothing is tagged or signed until it passed smoke". `release.yml` and the estate lanes find images by tag and trust them by signature, and an untagged, unsigned digest is invisible to both.

New obligation: a failed smoke leaves an **untagged orphan** in GHCR, and `ghcr-retention.yml` must prune untagged versions older than N days. Check it before landing; it is part of WI step 3.

Saves: the 137 s re-export plus about 100 s of the separate gitguard invocation's setup and cache import. That is **~3–4 min off `stack-image`** (11.0 → ~7.5). It also saves ~0.5 min on `image`, which uses the same candidate→push pattern.

### 2.3 Item 6: start the stack build earlier

`stack-image` waits for `image` only because the runtime stage does `COPY --from=core` of `CORE_IMAGE@<digest>`.

**Recommendation:** build core and stack in **one `docker buildx bake`**. Make core's Dockerfile a bake target, and give the stack target the named context `core = "target:core"`. BuildKit then runs core's stages concurrently with `web-build` and `server-build` in one graph.

- The `stack-image` job then needs only `[base-images, pod-base]`, so it starts at ~6.6 min instead of 8.0.
- Core is still pushed by digest, smoked (its "the image runs" step), tagged and signed as its own artefact, because `release.yml` and the estate lane pin `vogt@digest`. The `image` job either becomes a step inside this job, or stays as a thin "smoke + tag + sign core digest" job that `needs: stack-image`.
- **Provenance subtlety:** the stack's `/opt/vogt` must be the bytes of the pushed core digest. A bake build from the same source and cache graph produces the same layer for the same inputs, but the design asserts it rather than assuming it. A smoke step compares `vogt --version` and the commit label of the pushed core digest with the stack's `/opt/vogt`.
- A cheaper partial alternative: start `pod-base` without waiting for `base-images` when the pod-base tag already exists, using a lookup-only fast path. Small gain (~0.3 min); it is folded into WI step 5 as a stretch.

Saves **~1.5–2.5 min** (the 1.4 min core build and the 8.0 − 6.6 start gap now overlap).

### 2.4 Item 4: trigger the dev lane from a green `main` build

**Recommendation: a GitHub `workflow_run` workflow that triggers Woodpecker with a narrowly scoped token fetched from Infisical over OIDC, and the existing cron re-enabled as a fallback.**

- New `.github/workflows/dev-trigger.yml`:
  - `on: workflow_run: workflows: [build], types: [completed], branches: [main]`, with the job gated on `conclusion == 'success'` and `head_branch == 'main'`.
  - `runs-on: [self-hosted]`: the runners sit on the estate network and can reach Infisical at `100.92.54.45:8400` and `ci.indexarr.net`. Verify that from a runner in WI step 1.
  - `permissions: id-token: write`.
  - Steps:
    1. Obtain a GitHub OIDC token with audience `infisical`.
    2. Exchange it at Infisical's OIDC-auth login for a short-lived access token for machine identity `gha-vogt-dev-trigger`. The identity trusts issuer `https://token.actions.githubusercontent.com`, subject `repo:TheDancingDeveloper-org/vogt:ref:refs/heads/main`, and the claim `job_workflow_ref == TheDancingDeveloper-org/vogt/.github/workflows/dev-trigger.yml@refs/heads/main`. It has read access to one secret.
    3. Read `WOODPECKER_DEV_TRIGGER_TOKEN`.
    4. `POST https://ci.indexarr.net/api/repos/95/pipelines` with `{"branch":"main","variables":{"LANE":"dev","SOURCE_SHA":"<workflow_run.head_sha>"}}`.
  - It writes the Woodpecker pipeline URL to the step summary. It fails loudly but never retries in a loop.
- **Credential: no GitHub secret.** The Infisical identity id and the Infisical URL are non-secret repository `vars`. The token lives in Infisical `infrastructure/prod` next to `WOODPECKER_TOKEN`, under the new name `WOODPECKER_DEV_TRIGGER_TOKEN`.
  - It belongs to a **new dedicated Woodpecker user** (`vogt-dev-trigger`, a Forgejo bot account) with **write** (not admin) on `indexarr/vogt-estate` only. That is enough to start a manual pipeline, and it cannot touch secrets or other repositories.
  - The `sprooty` admin token is never used.
  - Register it in Cadastre.
  - The only thing that is unavoidable on the GitHub side is the identity id as a `var`, which is not a secret.
- **Fallback if the operator rejects Infisical OIDC:** a repository secret `WOODPECKER_DEV_TRIGGER_TOKEN` holding the same bot user's token. The blast radius is "can start vogt-estate pipelines", and the lane's own cosign gate still decides what deploys.
- **Woodpecker never trusts the trigger.** `SOURCE_SHA` is a hint: `resolve_images.py main` gains `--sha`, resolves the `sha-<short>` tags for that commit, and the build step still cosign-verifies every digest against the `build.yml@refs/heads/main` identity. A forged trigger can at most redeploy a signed main build.
- **Ordering and newest-wins.** Woodpecker has no concurrency groups, so `resolve` gets two checks:
  - It skips (`SKIP=1`) when `SOURCE_SHA` is not the current `main` tip *and* a newer build exists. The newer one has its own trigger coming.
  - It skips when `receipts/dev/latest.json` already records that SHA, which is today's idempotence.

  Two overlapping dev deploys are still possible in a merge burst. `vogt_deploy.py` refreshes Komodo's cache before writing, so the later pin wins. That is acceptable for dev, and WI step 7 adds a cheap guard: abort the deploy if a newer receipt landed meanwhile.
- **Re-enable the `track-main` cron (id 3, `*/10`)** as the safety net for a lost trigger. It is idempotent via `--skip-if-latest`. This is a one-click operator action that needs no code, so it can land first.
- **Rejected:** a cron alone at `*/1` or `*/2`. It needs no credential, but it produces 720–1440 pipelines a day of noise on repo 95 and still averages 0.5–1 min of latency.
- **Rejected:** a step inside `build.yml`. Newest-wins `cancel-in-progress` could kill the trigger mid-call, and `workflow_run` cleanly means "the whole run went green".

### 2.5 What the release lane keeps

`release.yml` (GitHub):

- Unchanged in principle: it builds nothing and promotes by digest.
- It gains a fourth image, `vogt-stack-workbench`, in `build-run` (the job-name list asserted by `tests/test_release_workflow.py`) and in `promote`. That covers the `imagetools create` semver tags, `cosign verify` against the `build.yml@refs/heads/main` identity, and re-signing under `release.yml@refs/tags/v*`.

vogt-estate `release.yml` (Woodpecker) keeps:

- **cosign verify** of core, stack, voice **and workbench** against the release identity. This is the trust boundary and is not negotiable.
- **The promotion gate** (`promote_gate.py`). The dev receipt must also record `workbench_image_digest`, and that digest must equal the release's, in addition to the existing core and stack equality checks.
- **Estate-digest existence**, and promotion with no build.
- **Break-glass** (`PROMOTE_GATE=override`) builds with the same thin Dockerfile FROM the release workbench digest, so it is now ~30 s instead of ~4.5 min.
- Deploy, smoke, receipt: unchanged. The release lane stays **manual**. Nothing here auto-promotes to prod, and vogt-prod and msp-agent are untouched by every step in this design.

## 3. Time savings

| Segment | Now (median) | After | Source of saving |
|---|---|---|---|
| `build.yml` merge → stack ready | 19.3 min | **~13–15 min** | Item 3 −3.5, item 6 −1.5–2.5 (estimates from the review's step timings) |
| Dev lane (Woodpecker) | 6.0 min (292 s; `build` 266 s) | **~1.3–1.8 min** | Item 2: `build` 266 s → 15–30 s. The release lane already shows deploy + smoke + receipt in 41–77 s |
| Human gap build → dev | unmeasured, often > all jobs | **~0.3–0.5 min** | Item 4 trigger latency (`workflow_run` start plus one API call) |
| **Merge → dev, machine time** | **~25 min** | **~15–17 min** | |
| Break-glass prod | ~5.5 min | ~1.5 min | Thin build |

New GitHub cost: the `stack-workbench-image` job takes ~2–3 min of runner time per main build, in parallel. There is also one workbench pod-base build a week (~10–15 min, Flutter plus Chrome), of which the estate lane currently pays the equivalent of about 4.4 min **on every deploy**.

## 4. Risks

| Risk | Likelihood / impact | Mitigation |
|---|---|---|
| The thin estate image is not equivalent to today's (missing tool, PATH, ownership, `/home` shadowing) | Med / High: in-pod agents lose Flutter or Chrome | WI step 6 runs a **shadow period**: the dev lane builds both the legacy and thin images for ≥3 runs. `ci/compare_estate_images.sh` diffs a tool manifest (`command -v` and `--version` for every tool, `CHROME_EXECUTABLE`, file lists under `/opt`, `/usr/local/bin`, uid/gid). The thin image is deployed only after zero diffs |
| Untagged orphans after a failed smoke (item 3) | High / Low | Retention prunes untagged versions; this is checked in WI step 3 |
| Bake core+stack changes core bytes or provenance (item 6) | Low / Med | Assert the core digest's `/opt/vogt` equals the stack's in the smoke. Keep the old `needs: image` path behind a workflow `env` toggle for one release |
| `publish` runner queue serialises `stack-image` and `stack-workbench-image` | Med / Med: the dev path gets slower | Measure after WI step 4. If the workbench job lands on the critical path, run both bake targets in one job on one runner (shared BuildKit graph), at the cost of more job time |
| The trigger credential leaks from a runner | Low / Low | It is a bot user with write on one repository. Infisical OIDC tokens are short-lived and bound to `dev-trigger.yml@main`. Cosign still gates what deploys |
| Infisical is unreachable from the GitHub runners | Low / Med | WI step 1 verifies this. The cron fallback still deploys within 10 min |
| Overlapping dev deploys in a merge burst | Med / Low (dev only) | Newer-receipt guard (WI step 7). Komodo cache refresh before write |
| A release tags a commit whose dev receipt predates workbench | One-off / Low | Gate step lands after ≥1 dev receipt carries `workbench_image_digest`; an older receipt fails the gate, and break-glass is the documented path |

## 5. Rollout and rollback order

Every step is independently revertible. Prod only ever receives a digest that a dev receipt vouches for, so a bad step stops at dev.

1. **Re-enable the `track-main` cron** (operator action in the Woodpecker UI/API). Rollback: disable it.
2. **Item 3: build once** (`build.yml`). Rollback: revert the commit. The tags and signatures consumers read are unchanged.
3. **Workbench pod-base variant + `stack-workbench-image` + release.yml promotes it.** These are additive, and nothing consumes them yet. Rollback: revert; the extra repository is harmless.
4. **Estate lane consumes workbench in shadow mode:** it builds both, diffs, and deploys **legacy**. The receipt starts recording `workbench_image_digest`. Rollback: revert vogt-estate.
5. **Flip the dev lane to deploy the thin image.** Keep `ESTATE_FLAVOUR=legacy` as a manual-run variable for one release cycle. Rollback: run the dev lane with `ESTATE_FLAVOUR=legacy`; prod is unaffected because it promotes only dev-vouched digests.
6. **Release lane: verify workbench and gate on it; break-glass uses the thin build.** Rollback: revert; the previous prod receipt and digest are re-promotable by re-running the last tag.
7. **Trigger** (Infisical OIDC identity + bot token + `dev-trigger.yml`). Rollback: `gh workflow disable dev-trigger.yml`, and the cron covers the gap.
8. **Item 6: bake core+stack.** Rollback: the env toggle back to `needs: image`, then revert.
9. *(Optional, later)* zero-build estate via runtime mounts.
10. Remove the legacy flavour after one clean release.

## 6. Backlog

The work items are listed below.

| WI | Step | Repo / owner | Depends on |
|---|---|---|---|
| WI-1648 | S1 re-enable `track-main` cron | operator (Woodpecker) | — |
| WI-1649 | S2 build once (item 3) | vogt | — |
| WI-1650 | S3 workbench pod-base + `vogt-stack-workbench` + release.yml | vogt | WI-1649 |
| WI-1651 | S4 thin estate Dockerfile, shadow + diff | vogt-estate | WI-1650 |
| WI-1652 | S5 dev lane deploys thin image | vogt-estate | WI-1651 |
| WI-1653 | S6 release lane verifies/gates workbench, thin break-glass | vogt-estate | WI-1652 |
| WI-1654 | S7a bot user + Infisical token + OIDC identity | operator / infra | — |
| WI-1655 | S7b `dev-trigger.yml` + resolve `--sha` guard | vogt + vogt-estate | WI-1654, WI-1648 |
| WI-1656 | S8 bake core+stack (item 6) | vogt | WI-1649 |
| WI-1657 | S9 remove legacy flavour | vogt-estate | WI-1653 |

The trigger chain (WI-1648, WI-1654, WI-1655) is independent of the image chain and can ship first.
