# WI-1058 — Port the brief

**Kind** feature · **State** in_progress · **Priority** p1 · **Effort** l · **Project** vogt · **Labels** rust, brief

## Description

Port the brief.

Keep it exact.

## Relations

- depends on WI-1053 — Pure domain III
- relates to wrk_gone

## Why this is ranked where it is

Score 84.4969, from:

- **priority** +55 (1 x 55) — p1
- **blocking_fan_out** +16 (2 x 8) — 2 item(s) depend on this one
- **staleness** +3.9969 (7.994 x 0.5) — 8.0 days since last change
- **trust_penalty** -3 (1 x -3) — trust state unverified

Not yet collected, so absent rather than zero:

- ci_red_boost — arrives with the CI check observations it reads

## Hand-off

- **branch** wi-1058-brief
- **head** abc1234
- **base** rust-core
- **session** ses_a
- **done** policy section
- **remaining** review brief
- **touch points** brief.rs
- **gates run** cargo test
- **blockers** none yet

Policy section ported; review brief next.

## Comments

- Remark after the hand-off.

## Project policy — vogt

- **Forbidden text** `secret-[0-9]+` — must not appear in a commit message, author, trailer, pull-request text or branch name.
- **Merges deploy production.** Merging a pull request onto `main` ships it. Do not run `gh pr merge` there; a person merges.
- **Commit identity** sprooty <sprooty@sprooty.com>
- **Tests** cargo test -p vogt-core
- Keep the lockfile.

## Landing rule for this repo

- **Base** `rust-core`
- **Branch pattern** `^wi-1058-[a-z0-9-]+$`
- **Lands by** push_branch
- **Merge by** oversight (rebase)
- **Deploy by** operator

## Deployment policy

The deployment's session policy — what an agent may do without asking — is the settings file this session was started with, not this brief.

## Before you start

This session is `ses_brief01`. It is bound to WI-1058; rebind with `session_bind_work` if you move on. How to record what you find, how to say you are blocked, and how to drive another session are in the guide beside this brief: the same file name, ending `.guide.md` instead of `.md`.
