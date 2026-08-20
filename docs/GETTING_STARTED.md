# Getting started with Vogt

This guide gets a new operator from a checkout to a working Python-core
instance. It does not require Rust, Node, a forge, an AI provider, MCP, or
Cadastre. Those integrations are optional and are documented separately.

## Choose an installation path

There are two supported ways to run the core:

- **Docker Compose** — the recommended self-hosting path. It gives you a
  persistent data volume, a health check, and a reproducible image build.
- **Local Python** — useful for development or a single-user workstation.
  It writes to the normal Vogt data directory unless you set
  `VOGT_DATA_DIR`.

The project also maintains a remote development deployment at
[`https://vogt-dev.sprooty.com/`](https://vogt-dev.sprooty.com/). A remote
instance still needs its own token; the URL does not grant access.

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

Start the core. `deploy/vogt.compose.yml` is the base and pulls a published
image. To build it from this checkout instead, add the build overlay:

```console
docker compose -f deploy/vogt.compose.yml -f deploy/vogt.build.yml up --build -d
```

Once the image is published and you are pinning it, the base runs on its own:

```console
docker compose -f deploy/vogt.compose.yml up -d
```

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
first. A healthy response includes the declared and observed schema
versions. Open
`http://localhost:8080/ui` in a browser to use the GUI.

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
and issue a scoped token from a trusted local process:

```console
uv run vogt token issue \
  --actor local:$(id -un) \
  --name browser \
  --scopes read,work.write,project.write \
  --reason "create a browser credential"
```

The secret is shown once. Store it in a file with restrictive permissions and
send it as `Authorization: Bearer ...`; never put it in a URL or command-line
argument. The Compose image uses the same token model when you expose it
beyond localhost.

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
`vogt-mcp-remote`. Do not install or configure Cadastre unless you have a
separate, explicit Cadastre deployment; the public Vogt image has no Cadastre
dependency and does not contact one.

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

- [`docs/USER_GUIDE.md`](USER_GUIDE.md) explains the GUI, ranked views,
  projects, drift, audit, and all supported CLI/API surfaces.
- [`docs/CONFIG.md`](CONFIG.md) is generated from the configuration schema.
- [`docs/CUSTOMISATION.md`](CUSTOMISATION.md) names the supported extension
  points — configuration, Compose overlays, image extension, your own front
  door — for a deployment that needs more than the base.
- [`docs/DEPLOYMENT.md`](DEPLOYMENT.md) covers network exposure, tokens,
  storage, backups, and operational topology.
- [`opensource.md`](../opensource.md) explains why the public path is
  Python-core-first and how the current private deployment is preserved.
