# Vogt

Vogt is a self-hosted product development environment for teams and the
agents that work with them. It combines project registration, work items,
ranked backlogs, repository observations, drift proposals, audit history, and
an HTTP API in one small Python application backed by SQLite.

The core is deliberately useful without a forge, an AI provider, a session
engine, or MCP. Plain folders and local Git repositories are first-class;
GitHub and agent integrations add capability when you opt in.

## Start here

The supported public delivery is the Python core image. The quickest path is
the generic Compose example:

```console
git clone https://github.com/TheDancingDeveloper-org/vogt.git
cd vogt
cp deploy/.env.example deploy/.env
docker compose -f deploy/vogt.compose.yml -f deploy/vogt.build.yml up --build -d
curl http://localhost:8080/health/ready
```

`deploy/vogt.compose.yml` is the base and pulls a published image;
`deploy/vogt.build.yml` is a one-service overlay that builds it from this
checkout instead. That pairing is the whole customisation model in miniature
— the base is never edited, and every deployment states only its difference
from it.

Open `http://localhost:8080/ui` after the readiness check succeeds. The full
walkthrough, including local Python installation, first project registration,
tokens, backup, and upgrades, is in
[`docs/GETTING_STARTED.md`](docs/GETTING_STARTED.md).

For the maintained development deployment used by the project, see
[`https://vogt-dev.sprooty.com/`](https://vogt-dev.sprooty.com/). It is an
example remote instance, not a default address embedded in the image or a
local installation.

## What is included

- A Python 3.11+ package and `vogt` CLI.
- A single FastAPI listener for the REST API, GUI, health endpoints, and
  optional Vogt MCP transport.
- SQLite storage with migrations, audit rows, event history, and backups.
- Project registration and import, local Git/source/dependency collectors,
  contract checks, observed-first backlog views, and drift proposals.
- Scoped bearer tokens for network access; every write requires a principal
  and a reason.
- An optional GitHub adapter. Without a GitHub token, the core remains fully
  functional and reports that forge observations were not collected.

The operation registry is the authority for the CLI, REST, and MCP surfaces.
Adding or removing a capability happens in the application and registry
layers, not in a GUI-only route.

## Principles

The short form of what makes this different from a ticket tracker. The long
form, with the reasoning, is in [`docs/DESIGN.md`](docs/DESIGN.md).

1. **Observed-first.** Work found in the wild — GitHub issues, marked TODOs,
   CI failures — is visible by default. Declaring it upgrades trust; it is
   never a precondition for being seen.
2. **Reports, never enforces.** Nothing takes compliance, trust, or drift as
   a precondition for an operation. Vogt tells you what is true and how old
   the answer is; you decide.
3. **Never goes looking.** Collection scope is the projects you registered.
   No crawling, no discovery.
4. **Declared and observed stay separated.** Collectors never silently mutate
   authoritative data; disagreement surfaces as drift.
5. **Every answer carries provenance and freshness.** "Verified 4 minutes ago
   from GitHub" and "declared 3 weeks ago, never confirmed" are different
   answers.
6. **Transport parity.** CLI, REST, MCP, and GUI are thin adapters over one
   operation registry, with tests asserting they agree.

## Optional integrations

MCP is an optional way to connect an agent to a running Vogt instance. It is
not needed to start Vogt, use the CLI, call the REST API, or open the GUI.
Cadastre MCP is not installed, configured, contacted, or required by the
public image or Compose example. Private deployments may configure it as a
separate integration when they need it.

Vogt is meant to be customised heavily, and
[`docs/CUSTOMISATION.md`](docs/CUSTOMISATION.md) names the supported extension
points: configuration, Compose overlays, image extension, running behind your
own front door, and the optional integrations above.

The Rust session engine and Capacitor mobile shell are separate components of
the wider Vogt work. They are not prerequisites for the Python core and are
not part of this quickstart — they are the largest worked example of the
extension points above. See [`opensource.md`](opensource.md) for the
public/private boundary and compatibility policy.

## Development

The Python core uses `uv`:

```console
uv sync
uv run pytest
uv run mypy
uv run ruff check .
uv run ruff format --check .
uv run python scripts/check_docs.py
```

Configuration reference files are generated from `src/vogt/config.py`:

```console
uv run python scripts/gen_config_docs.py
```

See [`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md) for the contribution
workflow and [`docs/CONFIG.md`](docs/CONFIG.md) for every setting.

## Documentation

- [Getting started](docs/GETTING_STARTED.md) — install, run, configure, and
  make the first project visible.
- [User guide](docs/USER_GUIDE.md) — daily use of the CLI, GUI, REST, and
  optional agent surfaces.
- [Configuration reference](docs/CONFIG.md) — generated schema reference.
- [Customisation](docs/CUSTOMISATION.md) — the supported extension points,
  and how to run a heavily customised deployment without forking.
- [Deployment guide](docs/DEPLOYMENT.md) — topology, storage, security,
  backups, and upgrade principles.
- [Design outline](docs/DESIGN.md) — architecture and domain decisions.
- [Requirements](docs/REQUIREMENTS.md) — complete requirements, revision
  rationale, delivery verification, and the gap register.
- [Open-source delivery direction](opensource.md) — the boundary and
  acceptance criteria for this transition.

## Licence

[MIT](LICENSE).
