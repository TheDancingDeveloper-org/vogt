# I7 swap: release, dev/prod deploy and rollback (WI-2507)

The plan for the 1.d flip: point a running stack at the Rust core, then
promote the same bytes. It is a plan. It does not flip anything, and it does
not remove the Python core. Both of those wait on the operator (WI-2505 is
the removal).

The image that carries both cores is the merged stack image. Its entrypoint
(`engine/deploy/entrypoint.sh`) reads `VOGT_CORE_IMPL` (`python` or `rust`,
default `python`) and runs that core on loopback, after retargeting the
`vogt` and `vogt-mcp-remote` names. Merging the Rust binary into the image is
not the cutover; setting the variable and redeploying is. Unset, or set to
anything else, the container stays on Python or refuses to boot — it never
guesses.

## What the flip actually is, per deployment shape

Two shapes run today, and the flip is a different change in each.

**The published stack** (`deploy/stack.compose.yml`, one container). The
core and the engine share a namespace, and the entrypoint supervises the
core. The flip is one environment line on the `vogt` service:

```yaml
VOGT_CORE_IMPL: "rust"
```

`deploy/stack.compose.yml` carries no `VOGT_CORE_IMPL` today, so the default
(`python`) applies. Add the line beside `VOGT_CORE_URL` (the block that
starts the core), defaulted off so the example stays on Python until an
operator opts in:

```yaml
# Which core the entrypoint supervises. `python` (default) or `rust`.
# The image ships both; flipping this and redeploying is the cutover,
# not the merge that put the Rust binary in the image. docs/rust-port/swap-release-plan.md.
VOGT_CORE_IMPL: "${VOGT_CORE_IMPL:-python}"
```

**The estate** (`indexarr/ops`, `personal/vogt-dev` and `personal/vogt-prod`).
This is a split stack, and the env var alone does nothing useful:

- The `engine` service runs `vogt-stack-estate` with
  `VOGT_CORE_URL: "http://vogt:8000"`. That names the sibling container, not
  loopback, so `entrypoint.sh` proxies and starts no core of its own. Setting
  `VOGT_CORE_IMPL` on the engine changes nothing that serves requests.
- The `vogt` service runs the standalone core image
  `ghcr.io/thedancingdeveloper-org/vogt@sha256:…` (pinned in
  `personal/vogt-{dev,prod}/vogt.compose.yml`). That image is the Python core
  only. It has no Rust binary and no `VOGT_CORE_IMPL`.

So the estate flip is a **core image pin**, made in ops, not in this
repository: replace the `vogt` service image with a core image that is the
Rust core (the published core artifact once it ships Rust, or the merged
image's `vogt-core` binary run as the core), keep the digest pin, and leave
the engine's `VOGT_CORE_URL` pointing at it. The exact ops change:

```yaml
# personal/vogt-dev/vogt.compose.yml, then personal/vogt-prod/vogt.compose.yml
services:
  vogt:
    image: ghcr.io/thedancingdeveloper-org/vogt@sha256:<rust-core digest>
```

Do that through the vogt-estate pipeline (below), which rewrites the pin and
commits it. Do not hand-edit a running container.

## Pre-flip checklist

Nothing below is optional. The flip is one redeploy; the way back is only as
good as what was true before it.

**Remaining work, all dependencies of WI-2507:**

| Item | What it gates |
| --- | --- |
| WI-2021 | in-process schedule: collector sweep and budget-stop settlement, as `adapters/http/scheduler.py` |
| WI-2660 | the gate itself: `VOGT_CORE_IMPL=python\|rust`, so merging rust-core to main is not the cutover |
| WI-2663 | `operation.run` must not block tokio workers; `/mcp` body bounded; the store mutex not poisonable |
| WI-2664 | `charter.get/set` and `watch.create/list/cancel` ported (currently stubbed 501) |
| WI-2665 | `session.report`, `usage.board`, `instance.diagnostics` ported; `initiative.publish` unstubbed on linked projects |
| WI-2506 | the docs rewritten for the Rust-only core, applied at the swap |
| WI-2505 | the swap PR: remove the Python core and rename `vogt-core` to `vogt` — after the flip has soaked, not before |

**Parity.** The Rust core answers what the Python core answers. The parity
jobs in `.github/workflows/rust-core.yml` are green on the commit being
flipped, and no golden was re-recorded to get there.

**End to end.** The stack smoke (`scripts/e2e_stack_smoke.sh`) passes against
the image with `VOGT_CORE_IMPL=rust`: both halves up, `/readyz` naming the
core, a read and a write through `/api/vogt`.

**Soak.** Dev runs the Rust core for a soak before prod is touched. A soak
here means the dev lane's live smoke passed and the instance then served
real sessions without a core-side 5xx or a migration error in the entrypoint
log. The length is the operator's call; the plan requires one, not a number.

**Schema.** Record the applied migration ids on dev before the flip
(`SELECT id FROM migrations` on both `declared.sqlite3` and
`observed.sqlite3` under the data dir). The flip must not require a
migration the rollback cannot undo. See rollback below.

## Release sequence

The release cadence does not change for the flip: build once, promote by
digest, with a hard dev gate. [`DEPLOYMENT.md`](../DEPLOYMENT.md) §9 is the
source for the generic release; this is the flip-specific ordering.

1. **Version bump on `main`.** A minor bump, landed like any change, so
   `build.yml` bakes the release version into the images.
2. **Build once.** `build.yml` publishes `vogt`, `vogt-stack` and `vogt-voice`
   as `sha-<commit>` with SBOM and provenance. No second build at tag time.
3. **Dev, and only dev.** Pin those digests on vogt-dev through the estate
   pipeline and validate. Prod is not touched in this step.
4. **Soak** on dev (above).
5. **Tag** `vX.Y.Z` on the same commit. `release.yml` retags the same digests
   `X.Y.Z`, `X.Y` and `latest`; it does not rebuild.
6. **Prod.** Promote the dev digests, not a freshly built set.

### Dev

Fire from outside vogt-dev — a lane recreates its stack and kills every
session inside it. From the workstation or a vogt-prod session, with the
`wp` tool (see the `vogt-estate-deploy` and `woodpecker` skills):

```console
N=$(wp trigger vogt-estate --lane dev | tail -1)
wp wait vogt-estate "$N"          # 0 success, 1 failure with the failed step's log
wp receipt dev                    # exit 1 unless live_smoke passed
```

The dev lane builds the estate image from the main SHA, rewrites the two
`@sha256` lines under `personal/vogt-dev`, deploys, smokes
`http://100.101.101.57:8910`, and commits `receipts/dev/<sha>.json` plus
`receipts/dev/latest.json`. For the flip, the core pin in
`personal/vogt-dev/vogt.compose.yml` moves to the Rust core image in the same
change (see the ops change above); the engine pin moves as it always does.

### Prod

Only after the dev receipt shows `live_smoke.status: passed` and the soak is
done. Promote dev's exact digests, do not rebuild:

```console
wp trigger vogt-estate --lane promote --var DRY_RUN=1        # prints the planned repin
N=$(wp trigger vogt-estate --lane promote --version vX.Y.Z | tail -1)
wp wait vogt-estate "$N"
wp receipt promote vX.Y.Z
```

The promote lane refuses unless the dev receipt passed smoke and carries the
core and estate digests, rewrites `personal/vogt-prod`, deploys, smokes
`http://100.92.54.45:18099`, and commits `receipts/prod/<version>.json` with
`promoted_from: dev`.

## Verify each step

| Step | Check | Pass looks like |
| --- | --- | --- |
| Image carries both cores | `docker exec <c> sh -c 'test -x /usr/local/bin/vogt-core && test -x /opt/vogt/.venv/bin/vogt'` | both executables present |
| Selection | entrypoint log line `vogt-core: implementation rust (…)`, or `python` on rollback | the implementation you set |
| Boot | `docker exec <c> sh -c 'wget -qO- http://127.0.0.1:8910/readyz'` | ready, core check present |
| Core identity | `docker exec <c> /opt/vogt/bin/vogt --version` | the Rust binary's version when flipped |
| Migrations | entrypoint log shows `init` applying nothing new, or names each applied id | no migration id you cannot reverse |
| Dev gate | `wp receipt dev` | `live_smoke.status: passed`, `source_sha` is the built commit |
| Prod | `wp receipt promote vX.Y.Z` | `promoted_from: dev`, digests equal the dev receipt |
| Same bytes | compare `images` in the dev receipt, the prod receipt and `vogt-release-manifest.json` | one digest per image, three ways |

## Rollback

The previous stack image tag stays deployable for one release, and the
release notes say so. Rollback is a redeploy of that tag, not a rebuild.

**During the overlap, prefer the env flip.** Both cores are in the image, so
the fastest rollback is `VOGT_CORE_IMPL=python` (the default) and a redeploy
of the *same* digest. No schema movement, no lost writes. This is the
rollback to rehearse.

**That only holds while the schema did not move.** Migrations are
forward-only: the migrator records the sha256 of every applied migration and
refuses a database that carries an id the running build does not have
([`DEPLOYMENT.md`](../DEPLOYMENT.md) §5). A Rust-core boot that applies a
migration the Python core lacks cannot be rolled back by redeploy alone.

**`observed/0007` is reversible by hand**, because it is one index and its
migration row:

```sql
-- against <data-dir>/observed.sqlite3, core stopped
DROP INDEX idx_sweeps_finished;
DELETE FROM migrations WHERE id = '0007_coverage_index';
```

`0007_coverage_index.sql` creates `idx_sweeps_finished` on
`sweeps (collector, finished_at, id)` and nothing else, so dropping the index
and the row restores the pre-0007 schema exactly. Take the backup first
(`vogt backup`); this statement is the exception, not the habit.

**Any later additive migration** — a new column, table or index shipped after
0007 — is reversed the same way only when it is purely additive: drop what it
created, in reverse order, then delete its `migrations` row. A migration that
backfills, renames or drops is not reversible in place. For one of those,
rollback means restoring the pre-flip backup and pinning the previous
release's digests, and every write made after the backup is lost. That is the
rule in [`DEPLOYMENT.md`](../DEPLOYMENT.md) §5, and it is why the pre-flip
checklist records the applied migration ids.

**Estate rollback** is the promote lane, not a hand edit:

```console
wp trigger vogt-estate --lane promote --var SOURCE_SHA=<previous dev sha>
```

The target must be a `receipts/dev/<sha>.json` whose live smoke passed and
whose digests are still in the registry. That redeploys the previous core
image (Python) and the previous estate image together.

## What this plan deliberately does not do

- It does not change the `VOGT_CORE_IMPL` default. The default stays
  `python` until the operator flips it.
- It does not remove the Python core, the `vogt` console script, or the
  standalone core image. WI-2505 does that, after the soak.
- It does not deploy. Deploying is the operator's, through the lanes above.
