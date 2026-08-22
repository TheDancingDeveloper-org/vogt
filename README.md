# Vogt

Vogt is a self-hosted product development environment for teams and the
agents that work with them. It combines project registration, work items,
ranked backlogs, repository observations, drift proposals, audit history, and
an HTTP API in one small Python application backed by SQLite.

The core is deliberately useful without a forge, an AI provider, a session
engine, or MCP. Plain folders and local Git repositories are first-class;
GitHub and agent integrations add capability when you opt in.

## Run it

The supported public delivery is the Python core image,
`ghcr.io/thedancingdeveloper-org/vogt`. Either path below gives you a
persistent data volume, a health check, and the GUI at `/ui`.

**Published image.** `deploy/vogt.compose.yml` is the base Compose file and
pulls the published image; `deploy/.env.example` holds the few values a host
has to state (public URL, port, bind address, image tag):

```console
git clone https://github.com/TheDancingDeveloper-org/vogt.git
cd vogt
cp deploy/.env.example deploy/.env     # set VOGT_PUBLIC_URL, VOGT_IMAGE
docker compose -f deploy/vogt.compose.yml up -d
curl http://localhost:8080/health/ready
```

**From source.** Add the one-service build overlay and the same base builds
the image from this checkout instead of pulling it:

```console
docker compose -f deploy/vogt.compose.yml -f deploy/vogt.build.yml up --build -d
```

Or skip containers altogether — the core is a plain Python 3.11+ package:

```console
uv sync
uv run vogt init
VOGT_PUBLIC_URL=http://127.0.0.1:8000 \
  uv run vogt serve --host 127.0.0.1 --port 8000 --no-auth
```

The base-plus-overlay pairing is the whole customisation model in miniature:
the base is never edited, and every deployment states only its difference from
it. The full walkthrough — first project, tokens, backup, upgrades — is in
[`docs/GETTING_STARTED.md`](docs/GETTING_STARTED.md); production concerns
(reverse proxy, TLS, backups, pinning, the optional engine) are in
[`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md).

## Dependencies and optional integrations

Everything except SQLite is optional, and absence is reported honestly rather
than failing startup. Core settings are `VOGT_*` environment variables or a
TOML file named by `VOGT_CONFIG_FILE`; the generated reference is
[`docs/CONFIG.md`](docs/CONFIG.md).

| Dependency | Required? | What it adds | Configured by |
|---|---|---|---|
| SQLite | built in | All storage: declared and observed stores, audit, events, backups. Nothing else to install. | `VOGT_DATA_DIR` (default `~/.local/share/vogt`) |
| GitHub token | optional | Issue/PR collection, forge links, and opt-in write-back. Without it the sweep records forge subjects as *not collected*, never as absent. | `VOGT_GITHUB_TOKEN_FILE` (a file, so the secret stays out of process listings); per-host `VOGT_FORGE_TOKEN_FILES` |
| Rust session engine + PWA | optional | PTY sessions over WebSocket, the Solid PWA, file/git APIs, agent tasks, push. Built from source with `engine/Dockerfile`; the core image does not contain it. | core side: `VOGT_ENGINE_URL`, `VOGT_ENGINE_TOKEN_FILE`, `VOGT_ENGINE_STATE_DIR`; engine side: `ENGINE_*` — see [`docs/ENGINE.md`](docs/ENGINE.md) |
| Voice/chat assistant provider | optional (engine only) | The assistant loop in the PWA. Any OpenAI-compatible chat endpoint works; unset key means the feature is off. | `ENGINE_ASSISTANT_BASE_URL`, `ENGINE_ASSISTANT_API_KEY`, `ENGINE_ASSISTANT_MODEL`; speech via `ENGINE_ASSISTANT_STT_*` / `ENGINE_ASSISTANT_TTS_*`. Legacy `MYDEVENV2_*` names are still accepted as aliases. |
| MCP | optional | Lets an agent drive the same operation registry: `vogt-mcp` (stdio, local data dir) or `vogt-mcp-remote` (bridge to a running instance). Not needed for CLI, REST, GUI, or health. | `VOGT_DATA_DIR` for `vogt-mcp`; `VOGT_URL` + `VOGT_TOKEN_FILE` for `vogt-mcp-remote` |
| External MCP integrations | optional | Other MCP servers (an infrastructure register such as Cadastre, LSPs, and so on) an operator wires into their agents alongside Vogt. The image installs, contacts, and requires none of them. | Your agent client's MCP configuration, not Vogt's |

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

## Customisation

Vogt is meant to be customised heavily, and
[`docs/CUSTOMISATION.md`](docs/CUSTOMISATION.md) names the supported extension
points: configuration, Compose overlays, image extension, running behind your
own front door, and the optional integrations above. The Rust session engine
and Capacitor mobile shell (`engine/`, `web/`, `mobile/`) are the largest
worked example of those extension points; they are not prerequisites for the
Python core. See [`opensource.md`](opensource.md) for the public/private
boundary and compatibility policy.

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
- [Deployment](docs/DEPLOYMENT.md) — production: images, Compose, environment,
  reverse proxy, backups, upgrades, the optional engine.
- [User guide](docs/USER_GUIDE.md) — daily use of the CLI, GUI, REST, and
  optional agent surfaces.
- [Configuration reference](docs/CONFIG.md) — generated schema reference.
- [Customisation](docs/CUSTOMISATION.md) — the supported extension points,
  and how to run a heavily customised deployment without forking.
- [Engine](docs/ENGINE.md) — the optional Rust session engine and PWA: what it
  owns, how to build and run it, its wire contract, the assistant.
- [Design outline](docs/DESIGN.md) — architecture and domain decisions.
- [Contributing](docs/CONTRIBUTING.md) — workflow and checks.
- [Public boundary and compatibility](opensource.md) — what is supported,
  what is optional, which legacy names remain as aliases.

## Licence

[MIT](LICENSE).
