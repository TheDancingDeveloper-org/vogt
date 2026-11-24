# Review WI-2 — Plain

**Kind** bug · **State** open · **Priority** p1 · **Project** vogt

## Review

You are reviewing this item, not carrying it out. Read the diff named below and check it against **Done means**, which is the item's own acceptance criteria. Report what holds and what does not, with the place in the diff each finding comes from. Do not implement the remaining work, do not resume the worker's session, and do not read the worker's screen or transcript — the diff and this brief are the whole of what you review.

This review is read-only. Make no edits to the branch or the checkout, and no commits, pushes, merges or deploys; do not open, approve or merge a pull request. Run the gates in a scratch worktree checked out at the head below. The one thing you write is your verdict.

## Done means

This item records no description, so it states no acceptance criteria. Say so in the review; do not invent ones.

## The diff

This item has no hand-off, so no branch or base was named and there is no diff to review yet. Report that and stop.

## Deployed setup

This project records no deployed-setup notes (`agent_policy.deployed_setup`). Say so in the verdict, and do not count an in-process test as covering how the project is deployed.

## Verdict

Post the verdict as one `work_comment` on this item. Its first line is exactly one of:

- `no blockers (head <sha>)` — every line of **Done means** holds at that head;
- `blockers (head <sha>):` followed by a numbered list, one blocker a line, each naming the line of **Done means** it fails and the place in the diff.

`<sha>` is the commit you reviewed. Then say **which deployment paths the tests cover**: in-process only, or the deployed topology described under **Deployed setup**. A test that passed in-process is not evidence for a path it never ran. Do not write `no blockers` while describing a regression or a gap — that is a blocker.

## Deployment policy

The deployment's session policy — what an agent may do without asking — is the settings file this session was started with, not this brief.

## Before you start

This session is `ses_brief01`. It is bound to WI-2; rebind with `session_bind_work` if you move on. How to record what you find, how to say you are blocked, and how to drive another session are in the guide beside this brief: the same file name, ending `.guide.md` instead of `.md`.
