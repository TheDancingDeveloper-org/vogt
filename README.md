# Vogt

Vogt is a self-hosted product development environment for teams and the
agents that work with them. It combines project registration, work items,
ranked backlogs, repository observations, drift proposals, audit history, and
an HTTP API, all backed by SQLite.

The product is a two-container stack: a small Python **core** that owns the
data and serves the API, and a Rust **engine** that fronts it — serving the
Solid PWA at `/`, owning the terminal sessions a work item runs in, and
proxying the core's API back through one published port. The core is also
useful on its own over the CLI, REST, and MCP, and reports missing
integrations honestly rather than failing startup; plain folders and local Git
repositories are first-class, and GitHub and agent integrations add capability
when you opt in.

## Run it

The stack is two Compose files layered together: the base
(`deploy/vogt.compose.yml`) runs the published core image,
`ghcr.io/thedancingdeveloper-org/vogt`, and the engine overlay
(`deploy/engine.overlay.yml`) adds the Rust engine in front of it. No engine
image is published, so the overlay always builds one from this checkout.

```console
git clone https://github.com/TheDancingDeveloper-org/vogt.git
cd vogt
cp deploy/.env.example deploy/.env     # set VOGT_PUBLIC_URL, the engine block
docker compose -f deploy/vogt.compose.yml -f deploy/engine.overlay.yml up --build -d
curl http://localhost:8080/health/ready
```

Once it is up, open the published URL in a browser: the engine serves the PWA
at `/`, and [`docs/USER_GUIDE.md`](docs/USER_GUIDE.md) is the tour of it. The
health check's `start_period` and the engine build both take a moment, so add
`--wait` to block until the stack reports healthy.

**Core alone.** The base runs the core on its own, without the engine — a
supported deployment for CLI, REST, and MCP use with no browser front end:

```console
cp deploy/.env.example deploy/.env     # set VOGT_PUBLIC_URL, VOGT_IMAGE
docker compose -f deploy/vogt.compose.yml up -d --wait
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
(reverse proxy, TLS, backups, pinning, the engine) are in
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
- A single FastAPI listener for the REST API, health endpoints, and optional
  Vogt MCP transport.
- SQLite storage with migrations, audit rows, event history, and backups.
- Project registration and import, local Git/source/dependency collectors,
  contract checks, observed-first backlog views, and drift proposals.
- Scoped bearer tokens for network access; every write requires a principal
  and a reason.
- An optional GitHub adapter. Without a GitHub token, the core remains fully
  functional and reports that forge observations were not collected.

The operation registry is the authority for the CLI, REST, and MCP surfaces,
and for the PWA that consumes them. Adding or removing a capability happens in
the application and registry layers, not in a client-only route.

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
6. **Transport parity.** CLI, REST, and MCP are thin adapters over one
   operation registry, with tests asserting they agree; the PWA is a client of
   that same REST surface and adds no capability of its own.

## Customisation

Vogt is meant to be customised heavily, and
[`docs/CUSTOMISATION.md`](docs/CUSTOMISATION.md) names the supported extension
points: configuration, Compose overlays, image extension, running behind your
own front door, and the optional integrations above. The Rust engine and
Capacitor mobile shell (`engine/`, `web/`, `mobile/`) are the largest worked
example of those extension points; the engine is the stack's front half, and
the mobile shell wraps its PWA. See [`opensource.md`](opensource.md) for the
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
- [Deployment](docs/DEPLOYMENT.md) — production: images, Compose, environment,
  reverse proxy, backups, upgrades, the optional engine.
- [User guide](docs/USER_GUIDE.md) — daily use of the PWA, CLI, REST, and
  optional agent surfaces.
- [Configuration reference](docs/CONFIG.md) — generated schema reference.
- [Customisation](docs/CUSTOMISATION.md) — the supported extension points,
  and how to run a heavily customised deployment without forking.
- [Engine](docs/ENGINE.md) — the Rust engine and PWA, the stack's front half:
  what it owns, how to build and run it, its wire contract, the assistant.
- [Design outline](docs/DESIGN.md) — architecture and domain decisions.
- [Contributing](docs/CONTRIBUTING.md) — workflow and checks.
- [Public boundary and compatibility](opensource.md) — what is supported,
  what is optional, which legacy names remain as aliases.

## Licence

[MIT](LICENSE).
