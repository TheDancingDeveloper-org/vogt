# Vogt

Vogt is a self-hosted product development environment for teams and the
agents that work with them. It combines project registration, work items,
ranked backlogs, repository observations, drift proposals, audit history, and
an HTTP API, all backed by SQLite.

The product ships as **one image**, `vogt-stack`: a small Python **core**
that owns the data and serves the API, and a Rust **engine** that fronts it —
serving the Solid PWA at `/`, owning the terminal sessions a work item runs
in, and carrying the `claude` and `codex` agent CLIs those sessions run. Pull
it, start it, open a browser. Everything beyond that is optional and reports
its absence honestly rather than failing startup: plain folders and local Git
repositories are first-class, and GitHub and agent integrations add
capability when you opt in.

## Live demo

Two public demo sites run the current build against seeded, read-only data —
the write API is isolated, so nothing there persists and no sign-in is needed:

- [**vogt-demo.thedancingdeveloper.com**](https://vogt-demo.thedancingdeveloper.com/)
  — the desktop Solid PWA.
- [**vogt-mobile-demo.thedancingdeveloper.com**](https://vogt-mobile-demo.thedancingdeveloper.com/)
  — the mobile app demo: a phone frame around the same responsive PWA that the
  Capacitor mobile shell wraps.

## Run it

One published image, no build. Copy the settings file, mint two secrets,
start it:

```console
git clone https://github.com/TheDancingDeveloper-org/vogt.git
cd vogt
cp deploy/stack.env.example deploy/.env      # set ENGINE_TOKEN
openssl rand -hex 32 > deploy/vogt-core-token
docker compose -f deploy/stack.compose.yml up -d --wait
curl http://localhost:8910/readyz
```

Open `http://localhost:8910/` and paste the `ENGINE_TOKEN` you chose into
**Settings (⚙)**. [`docs/GETTING_STARTED.md`](docs/GETTING_STARTED.md) takes
it from there — first project, tokens for agents, backup, upgrade — and
[`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md) covers running it somewhere real:
digest pinning, a reverse proxy, TLS, data.

Know what you are running. The image is a **development pod**, not a
hardened service: a writable home, passwordless `sudo`, an SSH server, and
the agent CLIs, because an agent session needs a machine and this is the
machine. It publishes on loopback until you say otherwise. Put it where you
would put a dev box, and put something that terminates TLS in front of it.

**Make it yours.** Vogt is meant to be customised, and the model is simple:
the published image is never edited, and your deployment states only its
difference from it. Settings go in `deploy/.env`; extra services, mounts and
secrets go in a Compose overlay of your own; extra tools go in an image of
your own that starts `FROM` the published digest. That last one is exactly
how the maintainer's own estate is built — the public image plus a few lines.
[`docs/CUSTOMISATION.md`](docs/CUSTOMISATION.md) is the guide.

**Working on Vogt itself?** The core is a plain Python package (`uv sync &&
uv run vogt init`), and the two-container developer stack — a core you build
beside an engine you build — is in
[`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md). Neither is a way to deploy
Vogt; they are ways to change it.

## Dependencies and optional integrations

Everything except SQLite and the engine is optional, and absence is reported
honestly rather than failing startup. Core settings are `VOGT_*` environment variables or a
TOML file named by `VOGT_CONFIG_FILE`; the generated reference is
[`docs/CONFIG.md`](docs/CONFIG.md).

| Dependency | Required? | What it adds | Configured by |
|---|---|---|---|
| SQLite | built in | All storage: declared and observed stores, audit, events, backups. Nothing else to install. | `VOGT_DATA_DIR` (default `~/.local/share/vogt`) |
| GitHub token | optional | Issue/PR collection, forge links, and opt-in write-back. Without it the sweep records forge subjects as *not collected*, never as absent. | `VOGT_GITHUB_TOKEN_FILE` (a file, so the secret stays out of process listings); per-host `VOGT_FORGE_TOKEN_FILES` |
| Rust session engine + PWA | in the image | PTY sessions over WebSocket, the Solid PWA, file/git APIs, agent tasks, push. The published image carries it wired to the core; the wiring is only yours to set when you run the halves apart. | engine side: `ENGINE_*` — see [`docs/ENGINE.md`](docs/ENGINE.md); core side, when split: `VOGT_ENGINE_URL`, `VOGT_ENGINE_TOKEN_FILE`, `VOGT_ENGINE_STATE_DIR` |
| Chat assistant provider | optional (engine only) | The assistant loop in the PWA. Any OpenAI-compatible chat endpoint works; unset key means the feature is off. | `ENGINE_ASSISTANT_BASE_URL`, `ENGINE_ASSISTANT_API_KEY`, `ENGINE_ASSISTANT_MODEL`. Legacy `MYDEVENV2_*` names are still accepted as aliases. |
| Voice (speech-to-text / text-to-speech) | in the stack, on by default | The stack bundles the first-party `vogt-voice` sidecar with baked default models, so the microphone transcribes and replies are spoken with no account. Any OpenAI-compatible audio endpoint can replace it. | `COMPOSE_PROFILES=voice` (clear it to run without the sidecar); repoint with `ENGINE_ASSISTANT_STT_*` / `ENGINE_ASSISTANT_TTS_*` — [`docs/CUSTOMISATION.md`](docs/CUSTOMISATION.md#voice-stttts) |
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

Development targets the `dev` branch: base branches on `origin/dev` and open
pull requests against it. `main` and `prod` are promotion-only release
branches. See [`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md) for the
contribution workflow and [`docs/CONFIG.md`](docs/CONFIG.md) for every
setting.

## Documentation

- [Getting started](docs/GETTING_STARTED.md) — install, run, configure, and
  make the first project visible.
- [Deployment](docs/DEPLOYMENT.md) — production: the image, Compose,
  environment, reverse proxy, backups, upgrades, releases.
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

[MIT](LICENSE). The terminal's bundled symbol-glyph fallback fonts under
`web/public/fonts/` are subsets of Noto Sans Symbols, Noto Sans Symbols 2 and
Noto Sans Math, licensed under the SIL Open Font License 1.1
(`web/public/fonts/OFL-NotoSans.txt`).
