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

## Live demo

Two public demo sites run the current build against seeded, read-only data —
the write API is isolated, so nothing there persists and no sign-in is needed:

- [**vogt-demo.thedancingdeveloper.com**](https://vogt-demo.thedancingdeveloper.com/)
  — the desktop Solid PWA.
- [**vogt-mobile-demo.thedancingdeveloper.com**](https://vogt-mobile-demo.thedancingdeveloper.com/)
  — the mobile app demo: a phone frame around the same responsive PWA that the
  Capacitor mobile shell wraps.

## Run it

There are three shapes, and they trade containment against convenience:

| | What it is | Web UI | Build |
|---|---|---|---|
| **Core only** | `deploy/vogt.compose.yml` — the API, hardened | no | no |
| **Core + engine** | the above + `deploy/engine.overlay.yml` | yes | the engine |
| **All-in-one** | `deploy/stack.compose.yml` — one published image | yes | no |

The first two keep the core in a hardened container: slim base, `read_only`,
all capabilities dropped. The all-in-one is a **development pod** — writable
home, `sudo`, sshd, and the `claude` and `codex` CLIs — because it exists to
run coding agents, and an agent session needs a machine. Pick it where you
would run a dev box, not where you would run a hardened service.
[`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md) §1.1 lays the three out in full.

No image of the engine alone is published, so the overlay path always builds
one from this checkout; the all-in-one image carries it with a core inside.

The quickstart below **builds the core from this checkout** with the
one-service build overlay `deploy/vogt.build.yml`, a path that works without
registry credentials. When using a published package, drop
`-f deploy/vogt.build.yml` and set `VOGT_IMAGE` to the release tag or digest.

```console
git clone https://github.com/TheDancingDeveloper-org/vogt.git
cd vogt
cp deploy/.env.example deploy/.env     # set VOGT_PUBLIC_URL
docker compose -f deploy/vogt.compose.yml -f deploy/vogt.build.yml up --build -d --wait
curl http://localhost:8080/health/ready
```

That runs the core on its own — the full API, CLI, and MCP, no browser front
end. `--wait` blocks until the healthcheck reports healthy; without it a curl
can race the idempotent `vogt init` bootstrap and the healthcheck's
`start_period`.

**Add the engine (the PWA).** The engine overlay builds the Rust engine from
this checkout and fronts the core with it, serving the Solid PWA at `/`; see
[`docs/USER_GUIDE.md`](docs/USER_GUIDE.md) for the tour. The engine image lifts
the core image in (`CORE_IMAGE`); to run against the core you build here
instead of the published image, layer all three files and name the local
core:

```console
VOGT_IMAGE=vogt:local docker compose \
  -f deploy/vogt.compose.yml -f deploy/vogt.build.yml -f deploy/engine.overlay.yml \
  up --build -d --wait
```

Fill in the engine block of `deploy/.env` first — the overlay's own header and
[`docs/ENGINE.md`](docs/ENGINE.md) list the keys.

**Once the `vogt` package is public**, the build overlay is optional: the base
pulls the published core and the engine overlay pulls it as `CORE_IMAGE` too,
so the stack is just the base plus the engine overlay. Set `VOGT_IMAGE` in
`deploy/.env` to the tag — or better, the digest — you intend to run:

```console
docker compose -f deploy/vogt.compose.yml -f deploy/engine.overlay.yml up --build -d --wait
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
the mobile shell wraps its PWA.

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
- [Agent guide](docs/AGENT_GUIDE.md) — for an agent running a stream of product
  work *through* Vogt: connecting, picking up work, linking branches and PRs
  back, and a drop-in block for your own repository.
- [Configuration reference](docs/CONFIG.md) — generated schema reference.
- [Customisation](docs/CUSTOMISATION.md) — the supported extension points,
  and how to run a heavily customised deployment without forking.
- [Engine](docs/ENGINE.md) — the Rust engine and PWA, the stack's front half:
  what it owns, how to build and run it, its wire contract, the assistant.
- [Design outline](docs/DESIGN.md) — architecture and domain decisions.
- [Contributing](docs/CONTRIBUTING.md) — workflow and checks.
- [AI policy](AI_POLICY.md) — how this stack is built: by AI agents, for
  AI-forward developers, and what that means for contributions.

## Licence

[MIT](LICENSE).
