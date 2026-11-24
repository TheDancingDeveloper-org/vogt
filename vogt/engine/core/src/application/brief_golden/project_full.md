# vogt

A terminal opened on this project. No work item is attached, so there is no task here beyond what you were asked for directly — when you take one up, call `session_bind_work` with its ref (and no `id`), so the item shows who is on it.

## Project policy — vogt

- **Forbidden text** `secret-[0-9]+` — must not appear in a commit message, author, trailer, pull-request text or branch name.
- **Merges deploy production.** Merging a pull request onto `main` ships it. Do not run `gh pr merge` there; a person merges.
- **Commit identity** sprooty <sprooty@sprooty.com>
- **Tests** cargo test -p vogt-core
- Keep the lockfile.

## Deployment policy

The deployment's session policy — what an agent may do without asking — is the settings file this session was started with, not this brief.

## Before you start

This session is `ses_brief01`. How to record what you find, how to say you are blocked, and how to drive another session are in the guide beside this brief: the same file name, ending `.guide.md` instead of `.md`.
