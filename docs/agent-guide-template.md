# Drop-in agent block for a Vogt-registered repository

Copy the block below into your own project's `AGENTS.md` or `CLAUDE.md`, and fill
in the two bracketed values (`<your-project-slug>` and `<instance-url>`). It tells
the agents working in your repository how to run product work *through* Vogt. The
reasoning behind each line is in [`AGENT_GUIDE.md`](AGENT_GUIDE.md).

```markdown
## Working through Vogt

This repository is registered in Vogt (`<your-project-slug>`), the work register
for this estate. Run product work *through* it:

- **Pick up work** with `backlog` / `bugs`, and read `why <ref>` before acting.
  Check each answer's provenance, trust and age — an unverified or stale
  observation is not a checked fact.
- **Claim it** with `work transition <ref> --to-state in_progress` and a real
  reason. Every write needs a reason; it lands in the audit log.
- **Branch** so Vogt can bind it: `wi-<n>` for work item WI-<n> (the default
  `branch_binding_template`; check this instance's CONFIG.md if the estate uses
  its own prefixes).
- **Link the PR back** with `Closes #<n>` in the title or body — Vogt reads the
  `implemented_by` edge and folds the PR under the item. It informs; it does not
  block completion.
- **Close it** with `work transition <ref> --to-state done` once the PR is
  merged. Only `depends_on` blocks completion; nothing else gates.
- **Vogt observes** branches, PRs, CI and drift on its own — you never type those
  in. **You declare** the work item, its state, its relations and its comments.
- Reach Vogt at `<instance-url>` via MCP (`vogt-mcp-remote`), REST (`/api/`,
  bearer token) or the `vogt` CLI. Run `vogt connect` for the exact client config.
```
