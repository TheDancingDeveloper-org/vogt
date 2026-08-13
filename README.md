# Vogt

*German: the reeve/bailiff who oversaw an estate, enforced its rules, and
answered for its work. Sits alongside
[cadastre](https://github.com/TheDancingDeveloper-org/cadastre), the land
register.*

**A product development environment for the AI era.** Jira-shaped in scope —
backlogs, bugs, state, CI visibility — but designed from the ground up for a
world where both humans and AI agents create, pick up, and complete work.

Vogt gives you:

- A **per-repo view**: state, backlog, open bugs, current version, CI status
  for any single project.
- A **global view**: open bugs and outstanding work rolled up across *all*
  projects, with an explainable priority ranking.
- **Contract checks**: ask whether a repo meets the project contract
  (required files, layout, metadata) and get back every failing criterion
  by name. It's a value you read, not a barrier you pass.
- **Full API surface**: everything the GUI can do, the CLI, REST API, and MCP
  server can do. Agents are first-class users.
- **Audit everything**: every write records who, what, and *why*.

## Status

**v1 complete: M0–M6 are implemented**, design revision r4. Every stage
demo runs as an acceptance test. Two must-have requirements are delivered
only in part and are named as such in `REQUIREMENTS.md` §5 — the `migrate`
CLI verb (FR-L1) and the audit log's time filter (FR-S6).

Vogt now sees work you did not type in. Collectors sweep your registered
projects for git state, source markers and dependency references — plus
GitHub issues, PRs, Actions runs and releases when you configure the
optional adapter — and the ranked backlog shows collected subjects alongside
declared work, ordered by the same weights, each stamped with how fresh the
evidence is and how much it is trusted. Every stage demo runs as an
acceptance test, including the one that unplugs GitHub entirely.

**M3 (Contract & drift) is also implemented**: the contract is checkable
on demand and reported with its age, and disagreements between declared
state and observation become drift proposals carrying their own evidence.

**M4 (Service) is implemented**: one process serving `/api`, `/mcp` and
plain-HTTP health on a single port, in-process TLS, scoped bearer tokens
bound to actors with double-gated writes, backup/restore, a hardened image
and a tag-triggered signing pipeline. The Node B stack is written and its
port allocated; publishing an image and moving production remain separate,
deliberate acts (NFR-D10).

**M5 (GitHub module) is implemented**: read-only consolidation of a
repository's existing issues, PRs, labels and releases; forge drift
(`forge_state_mismatch`, `vanished_upstream`, `ci_red_vs_healthy`,
`update_automation_gap`); and opt-in write-back that is additive and
forward-only — create, comment, label, close/reopen, and nothing else.

**M7 (Onboarding & inbox) is implemented**, post-v1: `project import` names
a GitHub repository, clones it into the configured import root, registers it
and consolidates its existing issues and PRs in one audited act — so a
repository that lives on GitHub is onboarded from GitHub rather than from a
local tree of unknown ancestry. Its notifications are collected per
repository, which scopes them to onboarded projects by construction, and read
through their own inbox rather than being mixed into the events feed.

**M6 (GUI) is implemented**, which completes v1: per-project view, global
backlog and bugs, drift inbox, dependency graph and audit browser, with trust
and freshness on every aggregate. Served at `/ui` from the same single port,
consuming only the public REST API — asserted by reading the shipped
JavaScript and resolving every URL in it against the operation registry.

See [`docs/DESIGN.md`](docs/DESIGN.md) for the outline,
[`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md) for the numbered baseline and
what has been deliberately deferred, [`docs/ROADMAP.md`](docs/ROADMAP.md) for
the stages (MVP = M0–M2) and what M0 actually shipped,
[`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md) for the topologies, and
[`docs/CONFIG.md`](docs/CONFIG.md) for the (generated) configuration
reference.

```console
$ uv run vogt init
$ uv run vogt project register --name Vogt --root-path . \
    --reason "the tracker's first project is itself"
$ uv run vogt sweep --reason "see what is already there"
$ uv run vogt backlog
$ uv run vogt why --ref WI-1
$ uv run vogt contract check --project vogt --reason "before release"
$ uv run vogt drift detect --reason "keep declared state honest"
```

Nothing crawls your filesystem: collection scope is the projects you
registered, and only those. Only markers matching a configured pattern
(`TODO(vogt):` by default) claim to be work — every other marker is still
observed and still queryable, it just does not fill your backlog. And a
noisy subject can be suppressed with a reason, which survives the next
sweep finding it again.

Every one of those is also a REST route and an MCP tool, generated from the
same registry — that is what the parity tests assert.

### Wiring an agent to it

`vogt-mcp` speaks MCP over stdio against the same data directory the CLI
uses, so no server is needed:

```json
{
  "mcpServers": {
    "vogt": {
      "command": "vogt-mcp",
      "env": { "VOGT_DATA_DIR": "/path/to/your/vogt" }
    }
  }
}
```

For a remote instance, `vogt-mcp-remote` bridges stdio to `/mcp` over
HTTPS, configured with `VOGT_URL` and `VOGT_TOKEN_FILE` — a file, never an
argument, so the token never reaches a process listing.

Every write an agent makes requires a `reason`, and lands an audit row and an
event carrying it. An agent's tool list is exactly what its token may do:
ungranted tools are absent rather than present-and-refusing.

## Principles (short form)

1. **Observed-first.** Work found in the wild (GitHub issues, marked TODOs,
   CI failures) is visible by default. Declaring/adopting it upgrades trust
   — it is never a precondition for visibility.
2. **Reports, never enforces.** Nothing in the system takes compliance,
   trust, or drift as a precondition for an operation. Vogt tells you what
   is true and how old the answer is; you decide.
3. **Never goes looking.** Collection scope is the projects you registered.
   No crawling, no discovery, no re-checking on a timer — ask, and the
   answer comes back stamped with its age.
4. **Declared vs observed, always separated.** Collectors never silently
   mutate authoritative data; disagreement surfaces as drift.
5. **Every answer carries provenance and freshness.** "Verified 4 minutes ago
   from GitHub" and "declared 3 weeks ago, never confirmed" are different
   answers.
6. **Transport parity.** CLI, REST, MCP, and GUI are thin adapters over one
   application layer, with tests asserting they agree.
7. **Writes are first-class.** Unlike a pure catalog, Vogt owns the
   write plane: workflow transitions, assignment, comments, and (opt-in)
   write-back to GitHub.
8. **Forge-optional.** The product is fully functional with no GitHub — no
   forge at all. Plain folders and local git are first-class; GitHub is an
   optional plugin that only ever *adds* observations and write-back.
9. **MCP by default.** Agents are the expected first users; MCP, REST, CLI,
   and GUI are generated peers over one operation registry.

## Stack

Python 3.11+ · SQLite (single-node, zero-dependency self-hosting) · FastAPI ·
MCP (stdio + streamable HTTP) · a buildless ES-module GUI (no Node
toolchain, no bundler output in version control — see `ROADMAP.md` M6) ·
optional GitHub + GitHub Actions integration (the only forge targeted in
v1). Self-hosted, open source.

Vogt self-hosts anywhere Docker runs. *This* estate's deployment (from M4)
is a Compose stack on Node B, deployed by Komodo from `indexarr/ops`, from
a signed GHCR image, reachable on the tailnet only — see
[`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md) §2.2.

## Licence

[MIT](LICENSE).
