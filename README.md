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
docker compose --env-file deploy/.env -f deploy/vogt.compose.yml up --build -d
curl http://localhost:8080/health/ready
```

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

## Optional integrations

MCP is an optional way to connect an agent to a running Vogt instance. It is
not needed to start Vogt, use the CLI, call the REST API, or open the GUI.
Cadastre MCP is not installed, configured, contacted, or required by the
public image or Compose example. Private deployments may configure it as a
separate integration when they need it.

The Rust session engine and Capacitor mobile shell are separate components of
the wider Vogt work. They are not prerequisites for the Python core and are
not part of this quickstart. See [`opensource.md`](opensource.md) for the
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
- [Deployment guide](docs/DEPLOYMENT.md) — topology, storage, security,
  backups, and upgrade principles.
- [Design outline](docs/DESIGN.md) — architecture and domain decisions.
- [Requirements](docs/REQUIREMENTS.md) — complete requirements, revision
  rationale, delivery verification, and the gap register.
- [Open-source delivery direction](opensource.md) — the boundary and
  acceptance criteria for this transition.

## Licence

[MIT](LICENSE).
