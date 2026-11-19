# Pod session completion: GitHub issue set (WI-4426)

This directory holds the work from the WI-4426 design as GitHub issue drafts. There is one file per issue. Each file is ready for `gh issue create --title … --body-file …`; `create-issues.sh` does that for the whole set.

- **Design:** [`design.md`](design.md) (the WI-4426 design doc). It holds the live evidence, the gap matrix with file:line references, the root causes and the target design.
- **Source baseline:** vogt graft line `rust-2` changeset `ce761a7` (git `rust-core` `e9a9cc19`, graft batch 194). Line numbers refer to that tree. Re-check them on the tip before editing.
- **Goal (operator, 2026-11-19):** on vogt-dev, every worker and review session runs as a k3s pod in `vogt-sessions`. Builds run in the session pod, never in the engine container. A session survives an engine restart or eviction with its workspace, credentials (including the graft session token) and conversation. Host placement is an explicit, capped exception. Oversight, interactive and shell pods are phase 2.

## Conventions for every issue

- **Landing:** lands on graft `vogt/rust-2` with `g land`. No git branches, no PRs. Ops issues land in Forgejo `indexarr/ops` by PR (exempt from graft).
- **Scope:** one area per issue. A worker edits only the files under **Files**. Anything else is a stop-and-report.
- **Done-when:** every numbered line must be true. A bug-fix test must be shown to fail without the fix.
- **Gates** (from `engine/`, `CARGO_BUILD_JOBS=4` on shared hosts, `VOGT_ENGINE_AGENT_TASK_PROMPT_FILE` unset):
  - `cargo fmt --check`
  - `cargo clippy -p <crate> --all-targets -- -D warnings`
  - `cargo nextest run -p <crate>`
- **Hand-off:** one Vogt `work_comment kind=handoff` on the linked WI, giving the changeset id, each done-when line with its file and test, the gates run, and a "verified/not verified" line. Then stop.
- **Deploys:** the operator approves every vogt-dev deploy and every engine restart.
- **Labels used:** `pods`, `phase-0`, `phase-1`, `phase-2`, `bug`, `feature`, `ops`, `docs`, `acceptance`, and `model:opus|sonnet|haiku`.

## Issue index

| File | Title | Phase | Model | Depends on | Vogt WI |
|---|---|---|---|---|---|
| [p0-1-engine-storage.md](p0-1-engine-storage.md) | ops: move vogt-dev engine /tmp and /cache off the container writable layer | 0 | sonnet | none | new (WI-4426) |
| [p1-01-landing-fields.md](p1-01-landing-fields.md) | brief.rs LandingFields accepts the full LandingPolicy | 1 | sonnet | none | WI-4417 |
| [p1-02-graft-sources.md](p1-02-graft-sources.md) | Declare graft sources on dev projects so pods get a minted graft token | 1 | haiku | P1.1 deployed | WI-3229 |
| [p1-03-heartbeat.md](p1-03-heartbeat.md) | Pod heartbeat survives sustained traffic | 1 | opus | none | WI-4418 |
| [p1-04-rust-image.md](p1-04-rust-image.md) | Pod image rust stage: clippy, rustfmt, rust-src, a read-only `stable` | 1 | sonnet | none | WI-3162 |
| [p1-05-account-leases.md](p1-05-account-leases.md) | Recovered-but-hibernated sessions hold no account slot | 1 | sonnet | none | new |
| [p1-06-pod-sizing.md](p1-06-pod-sizing.md) | Rust worker pod sizing from measurement | 1 | sonnet | P1.4 deployed | WI-3166 / WI-3181 |
| [p1-07-restart-readopt.md](p1-07-restart-readopt.md) | Engine restart with live pods: forced-kill path and test hardening | 1 | opus | P1.3 | new (WI-3205 follow-up) |
| [p1-08-host-placement-cap.md](p1-08-host-placement-cap.md) | Host placement is an explicit, capped exception | 1 | opus | P1.5 | new (WI-895 successor) |
| [p1-09-free-space-quota.md](p1-09-free-space-quota.md) | Engine free-space check sees its own ephemeral quota | 1 | sonnet | P1.8 | new |
| [p1-10-dev-config-docs.md](p1-10-dev-config-docs.md) | ops + docs: dev sends workers to pods | 1 | haiku | P1.8 deployed | WI-3169 (docs part) |
| [p1-11-wi3229-leftovers.md](p1-11-wi3229-leftovers.md) | WI-3229 leftovers: ENGINE.md pod boot, fixture, audit ignore | 1 | haiku | P1.2 | WI-3229 |
| [p1-12-acceptance.md](p1-12-acceptance.md) | Live acceptance: 5 Rust worker pods land and survive an engine restart | 1 | opus (oversight) | P0.1, P1.1–P1.8, P1.10 | WI-3169 |
| [phase-2.md](phase-2.md) | Phase 2 issues (sequenced, less detail) | 2 | various | phase 1 | WI-3370, 3164, 3354, 3162, 3170, 3171, 3159, 2698 |
| [closures.md](closures.md) | Items to close or narrow, with evidence | — | — | — | — |
| [decisions.md](decisions.md) | Operator decisions Q1–Q9 and the defaults the issues assume | — | — | — | — |

## Order and parallelism

```
P0.1 ─────────────────────────────────────────────┐
P1.1 ─(deploy)─ P1.2 ─ P1.11                      │
P1.3 ─ P1.7                                       ├─ P1.12 acceptance
P1.4 ─(deploy)─ P1.6                              │
P1.5 ─ P1.8 ─ P1.9                                │
            └─(deploy)─ P1.10 ────────────────────┘
```

- **Start in parallel:** P0.1, P1.1, P1.3, P1.4 and P1.5. Their areas are disjoint.
- **Never at the same time:** P1.5 and P1.8 (both edit `engine/server/src/sessions.rs`), and P1.3 and P1.7 (both touch the pod link and its liveness).
- **Two dev deploys come before acceptance:**
  1. after P1.1, P1.3, P1.4 and P1.5;
  2. after P1.7, P1.8 and P1.9.

## Bootstrap note for vogt-prod

These issues are about **vogt-dev**: its engine, its pods, and graft-dev line `rust-2`.

- Until P1.2 and the first deploy, dev pod sessions get **no graft token** ("the project names no graft source"). Run phase-1 code workers from vogt-prod with graft tokens minted by oversight, as today.
- Don't run vogt-dev builds on the dev engine host while P0.1 is pending. On 2026-11-19 the dev engine container was at about 5.7G of its 8Gi ephemeral limit 19 min after a restart.
