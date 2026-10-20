# Review WI-1058 — Port the brief

**Kind** feature · **State** in_progress · **Priority** p1 · **Project** vogt

## Review

You are reviewing this item, not carrying it out. Read the diff named below and check it against **Done means**, which is the item's own acceptance criteria. Report what holds and what does not, with the place in the diff each finding comes from. Do not implement the remaining work, do not resume the worker's session, and do not read the worker's screen or transcript — the diff and this brief are the whole of what you review.

This review is read-only. Make no edits to the branch or the checkout, and no commits, pushes, merges or deploys; do not open, approve or merge a pull request. Run the gates in a scratch worktree checked out at the head below, and remove that worktree (`git worktree remove --force`) and any build output you made outside it once the verdict is posted. The one thing you write is your verdict.

## Done means

Port the brief.

Keep it exact.

## The diff

- **branch** wi-1058-brief
- **head** abc1234
- **base** rust-core
- **session** ses_a

Review `git diff rust-core...abc1234` — branch `wi-1058-brief` against `rust-core` — and nothing older.

Check the branch tip first (`git rev-parse wi-1058-brief`). If it is not `abc1234`, review `abc1234` and say the branch has moved.

## What the worker claims

Everything in this section is the worker's hand-off: a claim, not evidence. Check each line against the diff and the gates you run yourself; do not repeat it as a finding.

- **done** (claimed)
  - claimed, not verified: policy section
- **remaining** (claimed)
  - claimed, not verified: review brief
- **touch points** (claimed)
  - claimed, not verified: brief.rs
- **gates run** (claimed)
  - claimed, not verified: cargo test
- **blockers** (claimed)
  - claimed, not verified: none yet

In its own words:

Policy section ported; review brief next.

## Gates

- **Tests** cargo test -p vogt-core

## Deployed setup

Engine and PWA run as separate services.

## Verdict

Post the verdict as one `work_comment` on this item. Its first line is exactly one of:

- `no blockers (head <sha>)` — every line of **Done means** holds at that head;
- `blockers (head <sha>):` followed by a numbered list, one blocker a line, each naming the line of **Done means** it fails and the place in the diff.

`<sha>` is the commit you reviewed. Then say **which deployment paths the tests cover**: in-process only, or the deployed topology described under **Deployed setup**. A test that passed in-process is not evidence for a path it never ran. Do not write `no blockers` while describing a regression or a gap — that is a blocker.

## Deployment policy

The deployment's session policy — what an agent may do without asking — is the settings file this session was started with, not this brief.

## Before you start

This session is `ses_brief01`. It is bound to WI-1058; rebind with `session_bind_work` if you move on. How to record what you find, how to say you are blocked, and how to drive another session are in the guide beside this brief: the same file name, ending `.guide.md` instead of `.md`.
