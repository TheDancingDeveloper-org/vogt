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

**M0 (Foundation) is implemented**; design revision r4. The storage spine,
audit and event tables, the transactional write path, the operation registry
and the transport-parity harness exist, and the M0 demo runs as an acceptance
test. M1 (the core tracker) is next.

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
$ uv run vogt status
$ uv run vogt events list
```

Every one of those is also a REST route and an MCP tool, generated from the
same registry — that is what the parity tests assert.

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
MCP (stdio + streamable HTTP) · React GUI · optional GitHub + GitHub Actions
integration (the only forge targeted in v1). Self-hosted, open source.

Vogt self-hosts anywhere Docker runs. *This* estate's deployment (from M4)
is a Compose stack on Node B, deployed by Komodo from `indexarr/ops`, from
a signed GHCR image, reachable on the tailnet only — see
[`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md) §2.2.

## Licence

[MIT](LICENSE).
