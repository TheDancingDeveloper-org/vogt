# vogt-dev work handed to vogt-prod (2026-11-19)

GitHub issue drafts, one markdown file per issue, with `title`/`labels` front matter and the issue body after it.

- [`pod-completion/`](pod-completion/README.md) — the WI-4426 plan to finish the pod session backend on vogt-dev. **Do this first.** It contains:
  - the design doc, with the gap matrix and root causes;
  - phase 0 and phase 1 issues, each with numbered done-when lines, file:line evidence and dependencies;
  - phase 2 in sequence;
  - closures;
  - operator decisions.
- **The full open backlog** of project `vogt` on vogt-dev (680 items, exported from Vogt with each item's body, relations and newest 4 comments) is in the private Forgejo repo `indexarr/vogt-estate` under `handoff/2026-11-19/backlog/`. It is kept out of this public repo because it carries estate internals: hosts, addresses and secret-store ids.

`create-issues.sh <owner/repo> pod-completion [--dry-run]` turns the drafts here into GitHub issues. The script copy in vogt-estate also takes `backlog` and `all`.

Vogt stays the system of record. Post hand-offs on the Vogt item each issue names.
