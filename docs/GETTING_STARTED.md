# Getting started with Vogt

This guide gets a new operator from nothing to a working Vogt — the browser
front end, the API, terminals, and the agent CLIs — with Docker as the only
prerequisite. A forge token, an AI provider, and an MCP client are all
optional and documented separately. Production concerns — a reverse proxy,
TLS, digest pinning, backups on a schedule — are in
[`docs/DEPLOYMENT.md`](DEPLOYMENT.md); this guide stops at a working
instance.

## Choose an installation path

There are two:

- **The published image (recommended)** — one container, pulled from the
  registry, nothing to build. It carries the core, the engine that serves the
  PWA at `/`, and the `claude` and `codex` CLIs. This is Vogt as it is meant
  to be run.
- **Local Python** — the core alone as a plain package, for development or a
  single-user workstation over the CLI, REST and MCP. No browser front end.
  It writes to the normal Vogt data directory unless you set `VOGT_DATA_DIR`.

## Docker Compose (recommended)

Prerequisites:

- Docker Engine 24 or newer with the Compose plugin;
- Git, to fetch the two deploy files (or copy them by hand); and
- a host port that is available for the web UI/API.

From the repository root:

```console
git clone https://github.com/TheDancingDeveloper-org/vogt.git
cd vogt
cp deploy/stack.env.example deploy/.env
openssl rand -hex 32 > deploy/vogt-core-token
```

Edit `deploy/.env` before starting. `ENGINE_TOKEN` is required — it is the
bearer token you will paste into the browser and hand to agents, at least 16
characters, and the stack refuses to invent one for you. Change `ENGINE_PORT`
if 8910 is already in use. The token file you just created is read by both
halves inside the container, which is what lets the engine talk to the core
on the first boot without a second deploy.

**Start it.** There is no `--build`: the image already carries everything.

```console
docker compose -f deploy/stack.compose.yml up -d --wait
```

`--wait` blocks until the healthcheck reports healthy. Without it, a curl
right after `up -d` can race the container: the core's `vogt init` runs
first, then the engine comes up, and the healthcheck's `start_period` is
60s, so an immediate probe can see connection-refused rather than a real
answer.

The port publishes to `127.0.0.1` unless you set `ENGINE_BIND`. The example
will not put a pod carrying `sudo` and agent CLIs on a network interface
because nobody said to.

Check it is up:

```console
curl http://localhost:8910/healthz     # the engine answers
curl http://localhost:8910/readyz      # ...and reports the core behind it
```

`/readyz` names each check — `vogt_core`, `workspace_agreement`,
`backup_agreement` — with a pass or fail and a reason. It deliberately stays
ready when the core is absent, because restarting the container would not
revive a core and would kill every live terminal; read the body, not just
the status.

Open `http://localhost:8910/` in a browser. The engine serves the PWA — the
board, backlog, terminals, agent tasks, and the voice assistant — at the
root. Open **Settings (⚙)**, paste the `ENGINE_TOKEN` you set, and save.
[`docs/USER_GUIDE.md`](USER_GUIDE.md) is the tour.

Stop or inspect the instance with:

```console
docker compose -f deploy/stack.compose.yml ps
docker compose -f deploy/stack.compose.yml logs -f vogt
docker compose -f deploy/stack.compose.yml down
```

Two named volumes survive `down`: `vogt-data` (the core's databases and
backups) and `engine-home` (the pod's home — agent state, session scratch,
the `Working` tree sessions run in). Do not add `--volumes` unless you
intentionally want to remove the instance and everything it holds.

The base file is never edited; every deployment states only its difference
from it as an overlay or an environment value. That is the whole
customisation model, and [`docs/CUSTOMISATION.md`](CUSTOMISATION.md) is the
long form.

## Local Python

Prerequisites:

- Python 3.11 or newer; and
- [`uv`](https://docs.astral.sh/uv/).

Install the development environment and initialise a local instance:

```console
uv sync
uv run vogt init
```

The default data directory is `~/.local/share/vogt` (or
`$XDG_DATA_HOME/vogt`). To choose another location:

```console
VOGT_DATA_DIR=/srv/vogt uv run vogt init
```

The CLI uses the same registry and application layer as the HTTP server. Run
commands from the checkout or set `VOGT_DATA_DIR` explicitly:

```console
uv run vogt status
uv run vogt project register \
  --name example \
  --root-path "$PWD" \
  --reason "register the checkout for observation"
uv run vogt sweep --reason "collect the initial repository evidence"
uv run vogt backlog
```

Collection is scoped to registered projects. Vogt does not crawl arbitrary
filesystem roots or discover projects on its own.

## Run the HTTP server locally

The server requires an explicit listen address and port. This prevents a
local default from accidentally becoming a network exposure:

```console
VOGT_PUBLIC_URL=http://127.0.0.1:8000 \
  uv run vogt serve --host 127.0.0.1 --port 8000 --no-auth
```

`--no-auth` is suitable only for a loopback listener. For a network listener,
start with authentication enabled (the default), initialise the instance,
and issue a scoped token from a trusted local process.

**First run (install mode).** A freshly initialised instance holds no tokens
at all, and while that is true the server is in *install mode*: `GET
/api/install/status` answers `{"install_mode": true}` and an unauthenticated
`POST /api/install/bootstrap` names the first operator and returns the first
token. This is what the browser first-run wizard rides, and it doubles as the
headless bootstrap for scripted installs:

```console
curl -s http://localhost:8080/api/install/bootstrap \
  -H 'Content-Type: application/json' \
  -d '{"display_name": "Ada Lovelace"}'
```

The answer carries the secret exactly once and an `admin`-scoped token bound
to the actor it just created (`human:ada-lovelace`); the write is audited to
that actor. The moment any token exists — this one, or one issued any other
way — install mode closes itself and the bootstrap refuses with
`install_closed`. Revoking every token does not reopen it: a lockout is fixed
from a trusted local process, below. The self-closing door is safe because
the port publishes on loopback by default (`VOGT_BIND_IP` falls back to
`127.0.0.1`); publish it to a real interface only after the first token
exists.

**Local (`uv run`).** The token is bound to the OS user running the command:

```console
uv run vogt token issue \
  --actor local:$(id -un) \
  --name browser \
  --scopes read,work.write,project.write \
  --reason "create a browser credential"
```

**Docker Compose.** The container runs as a fixed identity, `sprooty`, not
yours, so `local:$(id -un)` names an actor that was never created and the
command fails with `no actor with identity '...' — create it with 'actor
create' first`. `vogt init` bootstraps the actor `local:sprooty` inside the
container, so issue the token as that actor, from the container that owns
the data directory:

```console
docker compose -f deploy/stack.compose.yml exec vogt \
  vogt token issue \
  --actor local:sprooty \
  --name claude-code \
  --scopes read,work.write,project.write \
  --reason "create an agent credential"
```

This is the token for a CLI, an MCP client, or a script talking to the core
directly. The browser uses the engine's own `ENGINE_TOKEN` instead — the
engine is the front door, and it presents its own core credential behind the
scenes.

The secret is shown once. Store it in a file with restrictive permissions and
send it as `Authorization: Bearer ...`; never put it in a URL or command-line
argument.

## First project and first work item

Register the repository you want Vogt to observe, then sweep it:

```console
uv run vogt project register \
  --name my-project \
  --root-path /path/to/my-project \
  --reason "start tracking this repository"
uv run vogt sweep --reason "collect repository state"
uv run vogt project get --slug my-project
uv run vogt backlog
```

The default collectors read local Git state, configured source markers, and
dependency references. They return findings; the sweep records observations
and coverage. A collector cannot silently change declared work.

**Docker Compose.** `deploy/stack.compose.yml` mounts nothing from the host
by default, so `--root-path` above has nothing to observe until you bind-mount
a real checkout into the container. Mount it under the pod's `Working` tree —
that is both the core's import root and the root sessions open in, so a
project registered there is one a terminal can also be opened for. The pod
runs as uid 1000, so the host directory must be readable (and, for sessions,
writable) by that uid. See [`docs/CUSTOMISATION.md`, "Observing an estate on
a host path"](CUSTOMISATION.md#observing-an-estate-on-a-host-path) for the
full pattern. A minimal example:

```yaml
# my-overlay.yml
services:
  vogt:
    volumes:
      - /srv/my-project:/home/sprooty/Working/my-project:rw
```

```console
docker compose -f deploy/stack.compose.yml -f my-overlay.yml up -d --wait
docker compose -f deploy/stack.compose.yml exec vogt \
  vogt project register \
  --name my-project \
  --root-path /home/sprooty/Working/my-project \
  --reason "start tracking this repository"
docker compose -f deploy/stack.compose.yml exec vogt \
  vogt sweep --reason "collect repository state"
```

To create a new contract-shaped project instead:

```console
uv run vogt project create \
  --name new-project \
  --root-path /path/to/new-project \
  --reason "start a new tracked project"
```

Every mutating operation requires a reason and is written to the audit log.

## Optional GitHub integration

GitHub is an optional read/write-back module. The core remains complete when
no token is configured. To enable collection, place a fine-scoped GitHub token
in a file readable by the Vogt process and set:

```console
export VOGT_GITHUB_TOKEN_FILE=/secure/path/github-token
```

The absence of this file means “GitHub was not collected”, not “there are no
GitHub subjects”. See [`docs/CONFIG.md`](CONFIG.md) and the GitHub section of
the user guide before enabling write-back.

## Optional MCP integration

MCP is not required for startup or normal use. When you want to connect an
agent, use the built-in Vogt adapter after the server is running:

```console
VOGT_DATA_DIR=/path/to/vogt vogt-mcp
```

For a remote instance, configure `VOGT_URL` and `VOGT_TOKEN_FILE` for
`vogt-mcp-remote`. Other MCP servers you may run alongside Vogt in an agent
client — an infrastructure register such as Cadastre, a language server — are
your agent's configuration, not Vogt's; the public image installs, contacts,
and requires none of them.

## The session engine

The Rust session engine and its PWA (`engine/`, `web/`) are what give you
terminal sessions, file and git APIs, agent tasks, push notifications, and a
voice/chat assistant. The published image carries them already wired to the
core; nothing here needs configuring. Only the **Local Python** path runs
without them, and there the core simply reports that no engine is
configured. [`docs/ENGINE.md`](ENGINE.md) covers the engine's own settings;
the assistant provider is any OpenAI-compatible chat endpoint, configured
with `ENGINE_ASSISTANT_BASE_URL`, `ENGINE_ASSISTANT_API_KEY`, and
`ENGINE_ASSISTANT_MODEL`.

## Backup, upgrade, and removal

Use the lifecycle commands before upgrading a local or container deployment:

```console
uv run vogt backup --reason "backup before upgrade"
uv run vogt migrate
```

For Compose, take the backup from inside the running container:

```console
docker compose -f deploy/stack.compose.yml exec vogt \
  vogt backup --reason "backup before upgrade"
```

Upgrade by changing `VOGT_STACK_IMAGE` in `deploy/.env` to the new digest
(or tag), then `up -d --wait` with the same Compose file; startup applies
forward-only migrations. Keep the old image and backup until the new
readiness check and a restore test succeed.

To remove the example completely, first make a backup, then run:

```console
docker compose -f deploy/stack.compose.yml down --volumes
```

This removes the example's named data volume and cannot be undone by Docker.

## Where to go next

- [`docs/USER_GUIDE.md`](USER_GUIDE.md) explains the PWA, ranked views,
  projects, drift, audit, and all supported CLI/API surfaces.
- [`docs/AGENT_GUIDE.md`](AGENT_GUIDE.md) is for an agent running product work
  *through* Vogt — connecting over MCP/REST/CLI, picking up ranked work, linking
  branches and PRs back to items, and a drop-in block for your own repository.
- [`docs/CONFIG.md`](CONFIG.md) is generated from the configuration schema.
- [`docs/CUSTOMISATION.md`](CUSTOMISATION.md) names the supported extension
  points — configuration, Compose overlays, image extension, your own front
  door — for a deployment that needs more than the base.
- [`docs/DEPLOYMENT.md`](DEPLOYMENT.md) is the production guide — exposure,
  reverse proxy and TLS, digest pinning, backups, upgrades, and running the
  optional engine next to the core.
- [`docs/ENGINE.md`](ENGINE.md) is the optional session engine's reference.
- [`docs/DESIGN.md`](DESIGN.md) states the architecture and the product
  boundary: what the stack is, and which pieces are optional.
