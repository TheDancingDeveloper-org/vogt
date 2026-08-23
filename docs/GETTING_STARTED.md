# Getting started with Vogt

This guide gets a new operator from nothing to a working Vogt stack — the
Python core and the Rust engine that serves the PWA — with Docker as the only
prerequisite (it builds the engine for you). A forge token, an AI provider,
and an MCP client are all optional and documented separately. Production
concerns — a reverse proxy, TLS, digest pinning, backups on a schedule — are
in [`docs/DEPLOYMENT.md`](DEPLOYMENT.md); this guide stops at a working
instance.

## Choose an installation path

There are three supported ways to run Vogt:

- **The stack (recommended)** — the core plus the engine, two containers on
  one published port. Docker builds the engine from the checkout, so you need
  Docker and nothing else; the browser experience — the PWA at `/` — is the
  reason to run it.
- **Core only** — the base Compose file on its own, no engine and no browser
  front end. A supported deployment for CLI, REST, and MCP use.
- **Local Python** — the core as a plain package, useful for development or a
  single-user workstation. It writes to the normal Vogt data directory unless
  you set `VOGT_DATA_DIR`.

## Docker Compose (recommended)

Prerequisites:

- Docker Engine 24 or newer with the Compose plugin;
- Git, if building from a checkout; and
- a host port that is available for the web UI/API.

From the repository root:

```console
git clone https://github.com/TheDancingDeveloper-org/vogt.git
cd vogt
cp deploy/.env.example deploy/.env
```

Edit `deploy/.env` before starting. `VOGT_PUBLIC_URL` is required because a
container cannot infer the address clients will use. For a local installation,
the example value `http://localhost:8080` is correct. Change `VOGT_PORT` if
port 8080 is already in use.

**Start the stack (recommended).** Layer the engine overlay on the base. The
base pulls the published core image; the overlay builds the engine from this
checkout, since no engine image is published. Fill in the engine block of
`deploy/.env` first — the overlay's own header and [`docs/ENGINE.md`](ENGINE.md)
list the keys — then:

```console
docker compose -f deploy/vogt.compose.yml -f deploy/engine.overlay.yml up --build -d --wait
```

**Core only.** The base Compose file on its own runs the core without the
engine — no browser front end, but the full API, CLI, and MCP. There are two
ways to obtain the core image, and the same base serves both. Set `VOGT_IMAGE`
in `deploy/.env` to the tag — or better, the digest — you intend to run, then
pull the published image:

```console
docker compose -f deploy/vogt.compose.yml up -d --wait
```

Or add the one-service build overlay and the base builds the core from this
checkout instead of pulling it (`VOGT_IMAGE` is ignored):

```console
docker compose -f deploy/vogt.compose.yml -f deploy/vogt.build.yml up --build -d --wait
```

`--wait` blocks until the healthcheck reports healthy. Without it, the curls
below can race the container: `vogt init` runs first, the healthcheck's
`start_period` is 20s, and the engine build takes a moment, so a curl right
after a bare `up -d` can see connection-refused rather than a real answer.

The base is never edited; every deployment states only its difference from it
as an overlay or an environment value. That is the whole customisation model,
and [`docs/CUSTOMISATION.md`](CUSTOMISATION.md) is the long form.

The Compose command runs the idempotent `vogt init` bootstrap before serving,
so a new named volume is ready without a manual container shell step. It is
required rather than convenient: `serve` on an empty data directory answers
`/health/ready` with 503 and tells you to run `vogt init` first.

The port publishes to `127.0.0.1` unless you set `VOGT_BIND_IP`. The example
will not put an instance on a network interface because nobody said to.

Check both health endpoints:

```console
curl http://localhost:8080/health/live
curl http://localhost:8080/health/ready
```

Readiness reports whether both stores are migrated to this build's schema — it
does not migrate them, which is why the Compose command runs `vogt init`
first. A healthy response includes the declared and observed schema versions.

If you ran the stack, open `http://localhost:8080/` in a browser: the engine
serves the PWA — the board, backlog, terminals, agent tasks, and the voice
assistant — at the root. [`docs/USER_GUIDE.md`](USER_GUIDE.md) is the tour.
The core-only path publishes no browser front end; it answers the CLI, REST,
and MCP surfaces, and a browser at `/` gets nothing to render.

Stop or inspect the instance with:

```console
docker compose -f deploy/vogt.compose.yml ps
docker compose -f deploy/vogt.compose.yml logs -f vogt
docker compose -f deploy/vogt.compose.yml down
```

The named volume `vogt-data` survives `down`. Do not add `--volumes` unless
you intentionally want to remove the instance and everything it holds.

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

**Local (`uv run`).** The token is bound to the OS user running the command:

```console
uv run vogt token issue \
  --actor local:$(id -un) \
  --name browser \
  --scopes read,work.write,project.write \
  --reason "create a browser credential"
```

**Docker Compose.** The container always runs as a fixed identity, not
yours, so `local:$(id -un)` names an actor that was never created and the
command fails with `no actor with identity '...' — create it with 'actor
create' first`. `vogt init` bootstraps the actor `local:vogt` inside the
container (the image's default uid, 1000, maps to that username in
`/etc/passwd` — see the root `Dockerfile`), so issue the token as that actor,
from the container that owns the data directory:

```console
docker compose -f deploy/vogt.compose.yml exec vogt \
  vogt token issue \
  --actor local:vogt \
  --name browser \
  --scopes read,work.write,project.write \
  --reason "create a browser credential"
```

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

**Docker Compose.** `deploy/vogt.compose.yml` mounts nothing from the host by
default, so `--root-path` above has nothing to observe until you bind-mount a
real checkout into the container. Add a small overlay that mounts the
repository and sets the uid that owns it (`VOGT_UID` in `deploy/.env`, or
`user:` directly in the overlay) — see [`docs/CUSTOMISATION.md`, "Observing an
estate on a host path"](CUSTOMISATION.md#observing-an-estate-on-a-host-path)
for the full pattern. A minimal example:

```yaml
# my-overlay.yml
services:
  vogt:
    user: "1000:0"                     # the uid that owns /srv/my-project
    volumes:
      - /srv/my-project:/workspace/my-project:rw
```

```console
docker compose -f deploy/vogt.compose.yml -f my-overlay.yml up -d --wait
docker compose -f deploy/vogt.compose.yml exec vogt \
  vogt project register \
  --name my-project \
  --root-path /workspace/my-project \
  --reason "start tracking this repository"
docker compose -f deploy/vogt.compose.yml exec vogt \
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

## Optional session engine

The Rust session engine and its PWA (`engine/`, `web/`) add terminal
sessions, file and git APIs, agent tasks, push notifications, and a
voice/chat assistant. They are a separate image, built from source with
`engine/Dockerfile`, and the core reaches them only when `VOGT_ENGINE_URL`
and `VOGT_ENGINE_TOKEN_FILE` are set. Nothing in this guide needs them.
[`docs/ENGINE.md`](ENGINE.md) covers building and running the engine; the
assistant provider is any OpenAI-compatible chat endpoint, configured with
`ENGINE_ASSISTANT_BASE_URL`, `ENGINE_ASSISTANT_API_KEY`, and
`ENGINE_ASSISTANT_MODEL`.

## Backup, upgrade, and removal

Use the lifecycle commands before upgrading a local or container deployment:

```console
uv run vogt backup --reason "backup before upgrade"
uv run vogt migrate
```

For Compose, take a backup from inside the running service or stop it before
copying the `vogt-data` volume. Upgrade by rebuilding/pulling the new image,
then start the same Compose file; startup applies forward-only migrations.
Keep the old image and backup until the new readiness check and a restore
test succeed.

To remove the example completely, first make a backup, then run:

```console
docker compose -f deploy/vogt.compose.yml down --volumes
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
- [`opensource.md`](../opensource.md) states the public boundary: what is
  supported, what is optional, and which legacy names remain as aliases.
