# Deploying Vogt

How to run Vogt somewhere that is not your laptop: from the published image
or from source, what must be configured before it will start, what every
external dependency is for and what happens without it, and how to keep the
data safe across upgrades.

This is the operator's document. [`GETTING_STARTED.md`](GETTING_STARTED.md)
covers a first local run; [`CONFIG.md`](CONFIG.md) is the generated reference
for every core setting; [`CUSTOMISATION.md`](CUSTOMISATION.md) is how to layer
your own estate onto the published image; [`ENGINE.md`](ENGINE.md) is the
session engine in full.

## 1. What gets deployed

One image, `ghcr.io/thedancingdeveloper-org/vogt-stack`, and inside it two
processes on one published port:

```
vogt-engine  (the front door, :8910)
  ├── /               the Solid PWA
  ├── /api/...        sessions, terminals, files, git, agent tasks, push
  ├── /api/vogt/...   proxied to the core, with the core token injected
  ├── /mcp            proxied to the core
  └── /healthz, /readyz
vogt serve   (the core, loopback only, :8000 inside the container)
  ├── /api/...        REST (FastAPI; OpenAPI at /openapi.json, UI at /docs)
  ├── /mcp            MCP streamable HTTP transport
  ├── /health/live, /health/ready, /version
  └── collector scheduler (in-process background sweeps)
```

**The core** is the Python service that owns the data and serves the API.
**The engine** is the Rust server that embeds the web UI from `web/`, owns
the terminals a work item's session runs in, hosts the voice assistant, and
fronts the core. The image also carries the `claude` and `codex` agent CLIs
those sessions run. The entrypoint starts the core on loopback and supervises
both; the core is never published on its own port.

**The mobile shell** (optional) — a Capacitor wrapper under `mobile/` around
the same PWA. Nothing server-side depends on it. It loads the deployed front
door, so ordinary server and PWA releases reach installed phones without an
APK rebuild; the production APK procedure is in §7.1.

### 1.1 What you are running, and what you are not

The image is a **development pod**, deliberately. It exists to run coding
agents, and an agent session needs a machine: a writable home, a fixed
`sprooty` uid, passwordless `sudo`, an SSH server, and a Docker CLI for
talking to a socket you may choose to mount. It cannot run `read_only` and
does not drop capabilities. Treat it the way you would treat a dev box: keep
it on loopback or a private network, put something that terminates TLS in
front of it, and give it only the mounts and credentials its sessions need.

**Decision (2026-09-04).** Vogt is offered in this one shape. Earlier
releases also documented a *core-only* deployment — the Python core alone in
a hardened container, no UI and no sessions — and a *core + engine* pair of
containers that kept the core hardened and built the engine from a checkout.
Both are withdrawn as supported ways to run Vogt: three doors sent every
newcomer through the wrong one, and the containment the core-only shape
offered was containment of the half of the product nobody deploys alone. The
consequences, plainly:

- **The core image (`ghcr.io/thedancingdeveloper-org/vogt`) is still
  published**, because the stack image is built from it by digest and the
  release manifest records both. It is a build input, not a product. Nothing
  in this document tells you to deploy it.
- **The voice sidecar image (`ghcr.io/thedancingdeveloper-org/vogt-voice`) is
  part of the shape**, not a third door: `deploy/stack.compose.yml` runs it
  beside the pod so voice works out of the box (§5.2), it is published and
  signed by the same release as the stack and versioned with it, and it is
  never published to the host. Clearing `COMPOSE_PROFILES` leaves it out.
- **The two-container files (`deploy/vogt.compose.yml` and
  `deploy/engine.overlay.yml`) remain**, as the contributor's way to run a
  core and an engine they are changing, and as what the end-to-end suite
  drives in CI. §3 covers them under that heading.
- **There is no hardened deployment of Vogt.** If your posture needs one, the
  shape to build is the published image behind your own front door, with the
  sessions feature understood as what it is: arbitrary code running as the
  pod's user.

`engine/Dockerfile`'s header records why the core image and the pod are still
two images rather than one build: collapsing them would silently change what
the core image is under an unchanged name.

## 2. Run from the published image

The supported self-hosting path is the Compose base at
[`deploy/stack.compose.yml`](../deploy/stack.compose.yml).

```console
git clone https://github.com/TheDancingDeveloper-org/vogt
cd vogt
cp deploy/stack.env.example deploy/.env
$EDITOR deploy/.env                    # at minimum: ENGINE_TOKEN
openssl rand -hex 32 > deploy/vogt-core-token
docker compose -f deploy/stack.compose.yml up -d --wait
curl -fsS http://127.0.0.1:8910/readyz
```

What the base does, and why it does it that way:

- **There is no `--build`.** The image carries the core, the engine, the PWA
  and the agent CLIs. A deployment pulls a digest; it never compiles.
- **`vogt init` runs before `serve`**, inside the container, on every start.
  It is idempotent — it creates the instance on a new volume, brings an
  existing one forward, and leaves the audit history alone after that.
  `serve` migrates both stores before it accepts traffic, so an image
  carrying a new migration cannot come up ready against an old schema.
- **Host exposure defaults to loopback.** The port is published on
  `${ENGINE_BIND:-127.0.0.1}:${ENGINE_PORT:-8910}`. Set `ENGINE_BIND` to a
  real interface only when you mean to expose the instance, and read §6
  first.
- **`ENGINE_TOKEN` is required** and the base refuses to start without it. A
  token the file invented would be a token nobody knows they are trusting.
- **The core token is a file, not a variable.** `deploy/vogt-core-token` is
  mounted as a Compose secret both halves read: the engine presents it on
  `/api/vogt`, and the core adopts it at `init` as the actor and scopes in
  `.env`. That is the whole first-boot bootstrap — no mint-then-redeploy.
  Legitimately empty until you mint one; `/api/vogt` then answers 401.
- **The core is not published.** It listens on loopback inside the
  container, and the entrypoint refuses to start if `VOGT_CORE_URL` names
  anything else. The engine is the only way in.
- **Two named volumes, no host binds.** `vogt-data` holds the core's SQLite
  stores and backups; `engine-home` holds the pod's home — agent state,
  session scratch, the `Working` tree sessions run in. They are separate so
  the data outlives a pod you decide to reset.
- **The health check is the lifecycle runner** probing the engine's
  `/readyz`. That endpoint reports the core's state but deliberately stays
  ready when the core is absent: restarting the container would not revive a
  core and would kill every live terminal.

### 2.1 The `.env` file

| Variable | Required | Default | Meaning |
|---|---|---|---|
| `ENGINE_TOKEN` | yes | — | The engine's bearer token, ≥16 characters. What the browser and agents present. |
| `ENGINE_PORT` | no | `8910` | Host port the container's 8910 is published on. |
| `ENGINE_BIND` | no | `127.0.0.1` | Host interface the port is published on. |
| `ENGINE_PUBLIC_URL` | no | — | The URL clients reach the stack at. Set it once there is a stable one; `connect` renders against it. |
| `VOGT_STACK_IMAGE` | no | `ghcr.io/thedancingdeveloper-org/vogt-stack:0.5.4` | The image to run. Pin a digest (§2.2). |
| `VOGT_VOICE_IMAGE` | no | `ghcr.io/thedancingdeveloper-org/vogt-voice:0.5.4` | The bundled voice sidecar. Versioned with the stack — pin the same release as `VOGT_STACK_IMAGE` (§2.2). |
| `COMPOSE_PROFILES` | no | `voice` | Compose profiles to start. `voice` runs the speech sidecar (§5.2); clear it to run the stack without one — the voice controls stay present but inert. |
| `VOGT_BOOTSTRAP_CORE_TOKEN_ACTOR` | no | `agent:engine` | Who the adopted core token acts as. |
| `VOGT_BOOTSTRAP_CORE_TOKEN_SCOPES` | no | `read,work.write,project.write` | How much it may do. Everything in the pod can read the file, so this is the blast radius. |
| `VOGT_HOOKS_REQUIRED` | no | `false` | Whether a missing lifecycle hook bundle is fatal. |

Every other setting — the core's `VOGT_*` ([`CONFIG.md`](CONFIG.md)) and the
engine's `ENGINE_*` (§5.2) — can be added to the `environment:` block of an
overlay.

### 2.2 Pin a digest

The base names a tag so the example reads. A deployment should name a
digest, because a digest is the only form of "which image is this" that a
rebuild cannot silently change — publishing an image and moving a
deployment are separate acts, and the digest line is what moves one.

Pin the **release family** — `X.Y.Z`, or `latest` while you are trying it.
The same registry organisation also holds `vogt-stack-estate`, the
maintainer's own dev/prod pods; §7 says why that is a separate package and
why it is not a "fuller" release.

For a release, verify the keyless signature before starting Compose. Replace
`<digest>` with the exact release digest; this command performs an anonymous
registry read and constrains both the workflow identity and the GitHub OIDC
issuer:

```console
cosign verify \
  --certificate-identity-regexp '^https://github.com/TheDancingDeveloper-org/vogt/.github/workflows/release.yml@refs/tags/v[0-9].*$' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  ghcr.io/thedancingdeveloper-org/vogt-stack@sha256:<digest>
```

```console
docker buildx imagetools inspect ghcr.io/thedancingdeveloper-org/vogt-stack:0.5.4 \
  | grep -m1 Digest
# then, in deploy/.env:
VOGT_STACK_IMAGE=ghcr.io/thedancingdeveloper-org/vogt-stack@sha256:<digest>
```

The voice sidecar is a second image from the same release —
`ghcr.io/thedancingdeveloper-org/vogt-voice:0.5.4`, signed by the same
workflow identity, so the `cosign verify` above applies to it unchanged — and
`stack.compose.yml` names it beside the stack. Pin it the same way, to the
same release:

```console
docker buildx imagetools inspect ghcr.io/thedancingdeveloper-org/vogt-voice:0.5.4 \
  | grep -m1 Digest
# then, in deploy/.env:
VOGT_VOICE_IMAGE=ghcr.io/thedancingdeveloper-org/vogt-voice@sha256:<digest>
```

The two are versioned together; a stack from one release with a sidecar
from another is not a tested pair.

An upgrade is then a change to those two lines plus `docker compose up -d`
(§7.4); a rollback is the reverse, with one caveat about schema migrations
that §7.5 spells out.

### 2.3 The uid

The pod runs as a fixed user, `sprooty`, uid 1000, gid 1000. Named volumes
are seeded from the image so they arrive correctly owned; a host directory
you bind-mount arrives with the host's ownership, so anything a session must
write — a checkout under `Working`, say — has to be writable by uid 1000
([`CUSTOMISATION.md`](CUSTOMISATION.md#observing-an-estate-on-a-host-path)).
The uid is not a deploy-time choice; change it by extending the image.

### 2.4 A second instance on the same host

Supported, and exactly the case where every host-wide default collides.
Give the second instance its own Compose project name, port, public URL and
core token file:

```console
docker compose -p vogt-staging \
  --env-file deploy/staging.env \
  -f deploy/stack.compose.yml up -d --wait
```

The named volumes in the base are project-scoped, so a distinct `-p` already
separates the data. What you must still change per instance is
`ENGINE_PORT`, `ENGINE_PUBLIC_URL`, and any host path or explicitly `name:`d
volume your own overlay adds — two writers on one SQLite database does not
stop the deploy and shows up later looking like corruption.

## 3. Build from source (contributors)

Nothing in this section is a way to deploy Vogt. It is how you run a core or
an engine you are *changing*, and how CI's end-to-end suite brings the
product up from a checkout. The files it uses — `deploy/vogt.compose.yml`,
`deploy/vogt.build.yml`, `deploy/engine.overlay.yml` and `deploy/.env.example`
— are the two-container developer stack: the core in one container, the
engine built beside it in another.

### 3.1 The core

**With Compose.** The core base runs the published core image alone
(`VOGT_IMAGE`, defaulting to `ghcr.io/thedancingdeveloper-org/vogt:0.5.4`);
the build overlay swaps that for a build of the checkout and changes nothing
else:

```console
docker compose -f deploy/vogt.compose.yml -f deploy/vogt.build.yml up --build -d
```

**With plain Docker.** The root `Dockerfile` is a two-stage build on a
digest-pinned `python:3.13-slim`, installs from the committed `uv.lock`, and
deliberately avoids BuildKit-only features so it builds on the legacy
builder too:

```console
docker build -t vogt:local .
```

**Without a container.** The core is an ordinary Python 3.13 package managed
with [uv](https://docs.astral.sh/uv/). `git` must be on `PATH` — `project
import` and the git collector shell out to it.

```console
uv sync
export VOGT_DATA_DIR=/var/lib/vogt          # any directory; one instance per directory
export VOGT_PUBLIC_URL=https://vogt.example.com
uv run vogt init
uv run vogt serve --host 127.0.0.1 --port 8000
```

`serve` has no default host or port anywhere, including in the image: those
encode exposure, and the deployment is what is allowed to decide them. To
run it as a system service, wrap exactly that `init && serve` pair.

### 3.2 The session engine and PWA

No image of the engine *by itself* is published, so this path always builds
one from the checkout. The overlay `deploy/engine.overlay.yml` builds the
engine and wires it in front of the core base in one command:

```console
cp deploy/.env.example deploy/.env          # fill in the "session engine" block
openssl rand -hex 32 > deploy/vogt-core-token
docker compose -f deploy/vogt.compose.yml -f deploy/engine.overlay.yml up --build -d
```

The overlay builds `engine/Dockerfile` from the **repository root** (its
context includes `web/` and `src/`), publishes the engine on loopback, and
points it at the core over the Compose network. The only value you must set is
`ENGINE_TOKEN` (the engine's own bearer token, ≥16 characters); §5.2 lists the
rest, all optional. Like the base, it exposes nothing on a network interface
until you set `ENGINE_BIND`.

`CORE_IMAGE` is the published core, lifted whole into the engine image so that
one container carries both halves; the overlay passes it as a build arg,
defaulting to the same image the base runs. The engine then proxies to the
sibling core because `VOGT_CORE_URL` names it (§5.2) — it runs its own core
only when that URL is a loopback address. The engine's base image
(`POD_BASE_IMAGE`) is the dev-pod toolchain, which since #184 is defined in
`engine/Dockerfile.pod` (built once by `.github/workflows/pod-base.yml` as the
`vogt-pod-base` image and consumed by digest) — `engine/Dockerfile` installs
only the per-commit halves on top of it, so **adding a tool to the pod means
editing `engine/Dockerfile.pod`**, not `engine/Dockerfile`. The base image and
its toolchain build args are also covered in [`ENGINE.md`](ENGINE.md) §3, which
covers running the engine from `cargo` without a container.

The overlay exposes `VOGT_INSTALL_AI_CLIENTS`. When enabled, Codex and Claude
are baked into the image from the Renovate-managed `engine/agent-versions.env`
manifest, and that baked copy is the **baseline**. The image records the
resolved versions and checks them at every start. A stray copy in the
persisted home volume (`~/.npm-global/bin/<cli>` — what a CLI's own "update
now" leaves behind) would shadow the managed one; by default it is
**quarantined** (moved aside as `<path>.shadowed-<epoch>`, with a warning) and
the pod boots on the managed copy. `VOGT_AGENT_SHADOW_POLICY=fail` restores the
strict gate that refuses to start; `warn` leaves a deliberate user-local
override in place and only says so. A pod that could not move the stray still
refuses, since integrity could not be guaranteed.

**Runtime pin.** The version a pod actually runs is a deploy-time value
(#590). `VOGT_CLAUDE_CODE_VERSION` and `VOGT_CODEX_VERSION` in the
container's environment — the published `stack.compose.yml` and the
build-from-source overlay both pass them through from `deploy/.env`; an
image that bakes further CLIs lists their variables in its
`/usr/local/share/vogt/agent-clis.tools` — are read by the entrypoint at every
start. An exact
version that differs from the baked one is installed by
`vogt-agent-cli-install` into `/opt/vogt/agent-clis/<tool>/<version>` (the
`engine-agent-clis` named volume), smoke-checked the same way the image build
checks its own install (`claude --version` must print the pinned version), and
made current through a symlink that PATH prefers. Updating a CLI is therefore
an `.env` edit and `docker compose up -d engine`; rolling back is the same
edit in reverse, and a version already on the volume is switched to without
network access. Unset, or set to the baked version, means the image's copy —
also offline. A version that fails to install or fails its smoke check is
logged and the pod starts on whatever was current before. `latest` and
`stable` (npm dist-tags) are refused unless `VOGT_AGENT_CLI_ALLOW_DIST_TAGS=1`,
so floating is a deliberate opt-in rather than the default. The installer
writes `/opt/vogt/agent-clis/manifest` (`<tool>=<version>`), and
`vogt-verify-agent-clis` checks the active CLI against it at boot; the CLIs'
own updaters stay disabled (`DISABLE_UPDATES=1`), so the installer is the only
sanctioned writer.

An overlay that defines the engine service itself — rather than layering on
the published `stack.compose.yml` — inherits none of that passthrough: it must
name the three variables (`VOGT_CLAUDE_CODE_VERSION`, `VOGT_CODEX_VERSION`,
`VOGT_AGENT_CLI_ALLOW_DIST_TAGS`) in its own `environment:` and mount the
`engine-agent-clis` volume, or the pins never reach the entrypoint and the pod
silently keeps the baked copies. Carrying the pins as overridable defaults in
that overlay (`"${VOGT_CODEX_VERSION:-<version>}"`) keeps them in version
control beside the image digest. Because a pin is applied at container start,
the only proof that it took is the running pod: after a deploy, `GET
/api/agent-clis` reports each tool's active and baked version, and whatever
drives the deployment should fail when the active version is not the one it
set.

The same installer can be reached while the pod is running: `GET
/api/agent-clis` on the engine reports each tool's active, baked and (on
request) newest upstream version, `POST /api/agent-clis/{tool}` moves the pin
in place (`agent-clis-write` capability), and Vogt puts both on its own
surfaces as `agent_cli.list` / `agent_cli.update` — `vogt agent-cli list`, the
REST route, and the MCP tools an agent session holds — so a session can say
what it runs and an operator can update it with a reason attached. Settings →
Operational visibility shows the rows and a "newer version available" hint. A
move made this way lasts until the next container start re-applies whatever
the environment says, so a change meant to stick belongs in `.env` too. See
[`ENGINE.md`](ENGINE.md) §5 for the wire shape.

The flag defaults to `false`, so an engine you build yourself is CLI-free
unless you say otherwise. The published `vogt-stack` image *does* carry them:
the versions are the `engine/agent-versions.env` pins baked at build time,
and the release build runs both binaries before it publishes the digest — a
build arg nothing executes is a default waiting to be forgotten. An engine
built here without the flag leaves the `Claude Code (protected)` and `Codex
(protected)` session templates registered but unable to start, because the
binaries are simply not in the image.

#### Deployment-owned lifecycle hooks

The image contains the neutral `/usr/local/bin/vogt-lifecycle` runner, but no
operator scripts or credentials. A private Compose overlay may mount a
read-only hook bundle at `/run/vogt/hooks` with `pre-start.d`, `post-start.d`,
and `post-health.d` directories. Executable files run in lexical order with
`VOGT_LIFECYCLE_PHASE`, `VOGT_LIFECYCLE_HOOK_DIR`,
`VOGT_LIFECYCLE_WORKDIR`, and `VOGT_LIFECYCLE_FIRST_START` set. A non-zero
pre/post-start hook stops startup; a non-zero post-health hook fails health.
Post-health runs on each probe and must be idempotent. Set
`VOGT_HOOKS_REQUIRED=true` when the bundle is mandatory; otherwise the public
base remains functional without a mount. Put lifecycle state on the data/home
volume to distinguish first restore from restart. The generic sample
`deploy/lifecycle-hooks/restore-and-verify.sh` refuses dirty checkouts and
verifies a required asset; provide its paths only in your private overlay.

## 4. Configuration

All core settings live in one schema, `src/vogt/config.py`, from which
[`CONFIG.md`](CONFIG.md) and `config.example.toml` are generated. Precedence,
highest first: command-line arguments, `VOGT_*` environment variables, the
TOML file named by `VOGT_CONFIG_FILE`, schema defaults.

The values you must decide before a network-facing deployment:

| Setting | Why you must set it |
|---|---|
| `ENGINE_PUBLIC_URL` (fronted) or `VOGT_PUBLIC_URL` (core alone) | An exposure value with no default. `connect` and `/connection-info` render client configuration against it. |
| `--host` / `--port` on `serve`, and `ENGINE_BIND` / `ENGINE_PORT` in Compose | Also exposure; nothing in the image will bind an address for you. |
| `VOGT_DATA_DIR` | Allocation, so it defaults (`/var/lib/vogt` in the image). One instance per directory. |

### 4.1 Tokens

The server authenticates every request by default; `--no-auth` exists for a
loopback listener and nothing else. A token is bound to an actor, carries
scopes (`read`, `work.write`, `project.write`, `admin`, `writeback`), and is
minted from the container that owns the data directory:

```console
docker compose -f deploy/stack.compose.yml exec vogt \
  vogt token issue --actor local:sprooty --name claude-code \
  --scopes read,work.write --reason "first agent credential"
```

The secret is shown once. Store it in a file and hand clients the file path —
`vogt-mcp-remote` reads `VOGT_URL` and `VOGT_TOKEN_FILE`, and every token
the core itself holds is configured as a `*_file` path for the same reason:
a token in the environment is a token in every `docker inspect`.

`vogt connect --format markdown` renders the connection document for a
running instance — URL, `/mcp` path, supported protocol versions, and a
client configuration — so nothing about the address is hand-copied.

## 5. Dependencies and integrations

Every external dependency is optional, is off until configured, and reports
its absence rather than faking a result. A Vogt with all of them off is a
complete product.

### 5.1 Core integrations

| Integration | Purpose | Optional | When absent | Configured by |
|---|---|---|---|---|
| **Forge providers** | Collects issues, pull requests, checks, labels and releases for registered projects; enables write-back and `forge_*` operations | yes | Subjects for hosts without a usable token read as *not collected*, never as absent; unsupported capabilities report their gap; write-back and account linking are unavailable for that host | `VOGT_GITHUB_TOKEN_FILE` (a path) for GitHub; other Forgejo/Gitea hosts under `forge_token_files` in TOML. Per-actor linked tokens: `VOGT_FORGE_ACCOUNT_KEY_FILE` |
| **Session engine** | Opens and attaches terminals for work items; the source of `session.*` | yes | `session.*` operations report "no engine configured"; nothing else changes | `VOGT_ENGINE_URL`, `VOGT_ENGINE_TOKEN_FILE`, optionally `VOGT_ENGINE_STATE_DIR` (so `backup` covers engine state) and `VOGT_SESSION_SCRATCH_PROJECT` |
| **MCP** (in-process) | Agents talk to this instance at `/mcp` on the same port, or over stdio via `vogt-mcp` | yes | Agents cannot connect; REST, CLI and GUI are unaffected | Nothing to enable; a token per client (§4.1) |
| **Remote MCP bridge** | `vogt-mcp-remote`, for clients that can only spawn a local process | yes | Use the HTTP transport instead — it needs nothing installed | `VOGT_URL`, `VOGT_TOKEN_FILE` in the client's environment |
| **A front door** | Anything that publishes the core at another address and paths (§6) | yes | Clients reach the core directly | `VOGT_FRONTED=true`; door sends `X-Vogt-Public-Url`, `X-Vogt-Api-Path`, `X-Vogt-Mcp-Path` |
| **Bootstrap core token** | Lets a fronted deployment come up in one deploy instead of mint-then-redeploy | yes | The front door's `/api/vogt` answers 401 until a token is minted by hand | `VOGT_BOOTSTRAP_CORE_TOKEN_FILE`, `_ACTOR`, `_SCOPES` |

Beyond these the core needs only SQLite (bundled with Python) and `git`
(in the image). There is no external database, cache or queue.

### 5.2 Engine integrations

These are read by the engine process, not the core. `deploy/stack.compose.yml`
wires the required ones from `deploy/.env`; the rest go in the `environment:`
block of an overlay of yours. `ENGINE_*` is the
current prefix; legacy `MYDEVENV2_*` names are still accepted as aliases for
one release and log a warning. Each can also be set in the engine's TOML config
file under the same name without the prefix ([`ENGINE.md`](ENGINE.md) §3).

| Integration | Purpose | Optional | When absent | Configured by |
|---|---|---|---|---|
| **Engine token and bind** | The engine's own bearer token (≥16 chars) and listen address | token required | The engine refuses to start | `--token`, `--bind` (default `127.0.0.1:8910`) and `--config` flags, or `ENGINE_TOKEN` / `ENGINE_BIND` / `ENGINE_CONFIG` in the environment (legacy `MYDEVENV2_TOKEN` / `MYDEVENV2_BIND` / `MYDEVENV2_CONFIG` still accepted with a deprecation warning), or the config file. Scoped extra tokens: `ENGINE_EXTRA_TOKENS_JSON`; write-rate cap: `ENGINE_MUTATING_REQUEST_LIMIT_PER_MINUTE` |
| **The core behind it** | Proxies `/api/vogt` and `/mcp` to a core, injecting the core token for `/api/vogt` | yes | The engine runs alone; `/readyz` reports the core's state and stays ready (an absent core must not cost running terminals); Vogt routes answer 503 naming the reason | `VOGT_CORE_URL` (loopback → the entrypoint also *runs* the core there; anything else → proxy only), `VOGT_CORE_TOKEN_FILE` (preferred) or `VOGT_CORE_TOKEN`, `ENGINE_PUBLIC_URL`, `VOGT_IMPORT_ROOT`, `VOGT_ENGINE_STATE_DIR` |
| **Voice assistant provider** | The chat model behind the assistant tab and spoken requests | yes | The assistant routes answer 404 and the PWA hides the tab | `ENGINE_ASSISTANT_API_KEY`, `ENGINE_ASSISTANT_BASE_URL`, `ENGINE_ASSISTANT_MODEL` — any **OpenAI-compatible chat endpoint**. A key with no base URL is a startup error, not a silent default provider. Tuning: `ENGINE_ASSISTANT_MAX_TOOL_CALLS`, `ENGINE_ASSISTANT_REASONING_EFFORT`, `ENGINE_ASSISTANT_LOG_RETENTION_DAYS`; several providers at once: `ENGINE_ASSISTANT_PROFILES_JSON`, `ENGINE_ASSISTANT_DEFAULT_PROFILE` |
| **Speech-to-text** | Server-side transcription for voice input | on by default | Voice input is unavailable; text chat unaffected | Provided by the bundled `voice` sidecar out of the box (`COMPOSE_PROFILES=voice`); the stack points `ENGINE_ASSISTANT_STT_BASE_URLS` at it. Repoint at any OpenAI-compatible audio endpoint with `ENGINE_ASSISTANT_STT_BASE_URLS` (comma-separated, ordered fallback — empty means off), `ENGINE_ASSISTANT_STT_API_KEY`, `ENGINE_ASSISTANT_STT_MODEL` (default `whisper-1`), and clear the profile to stop the sidecar |
| **Text-to-speech** | Spoken replies | on by default | Replies are text only | Provided by the bundled `voice` sidecar out of the box. Repoint with `ENGINE_ASSISTANT_TTS_BASE_URLS`, `ENGINE_ASSISTANT_TTS_API_KEY`, `ENGINE_ASSISTANT_TTS_MODEL` (default `tts-1-hd`), `ENGINE_ASSISTANT_TTS_VOICE` (default `nova`), `ENGINE_ASSISTANT_SPEECH_TIMEOUT_MS`. `ENGINE_ASSISTANT_TTS_FORMAT` is the `response_format` requested (default `mp3`; the stack sets `wav`, the only format the bundled Piper backend serves; the engine passes the upstream content type through). The stack also sets `ENGINE_ASSISTANT_TTS_MODEL=tts-1` and `ENGINE_ASSISTANT_TTS_VOICE=alloy`, the names the bundled sidecar advertises; the `tts-1-hd` / `nova` defaults are for a cloud provider |
| **Push notifications** | Web push to browsers; native push via Firebase Cloud Messaging | yes | Web push works with no configuration beyond a sensible `ENGINE_VAPID_SUBJECT`; without FCM only the native transport is disabled | `ENGINE_VAPID_SUBJECT` (a `mailto:`, default `mailto:admin@example.invalid`), `ENGINE_FCM_SERVICE_ACCOUNT_FILE` (a path; the inline `ENGINE_FCM_SERVICE_ACCOUNT_JSON` still works but puts a private key in the environment) |
| **Browser origins** | CORS allow-list for the PWA | yes | Only same-origin use | `ENGINE_ALLOWED_ORIGINS` |
| **GUI streaming** | An iframed remote desktop inside the PWA | yes | `/readyz` reports `gui: disabled`; the affordance is withdrawn with a reason | `GUI_STREAM_URL`, `GUI_STREAM_VERIFIED` |
| **External MCP servers for agents** | Extra MCP servers registered into a session's agent (for example an infrastructure register such as Cadastre) | yes | Agents inside a session see only Vogt's own MCP server | Engine image build args (`INSTALL_CADASTRE_MCP`) and the scripts in `engine/deploy/`; see [`ENGINE.md`](ENGINE.md) §4 and §9 |
| **Agent service auth** | Brokers third-party credentials into a session on demand | yes | Sessions run without pre-loaded credentials; nothing in the core product depends on it | `ENGINE_AUTO_AGENT_AUTH`, `ENGINE_AGENT_AUTH_HELPER` — maintainer-specific tooling, described in the script headers under `engine/deploy/` |

The authoritative list is `engine/server/src/config.rs`; when a variable is
not there, it does not exist.

## 6. Behind a reverse proxy

Two shapes work, and they differ in who states the public identity.

**Plain reverse proxy, same paths.** Caddy, nginx, Traefik or similar
terminating TLS and forwarding to the published port. Vogt needs nothing
special: set `VOGT_PUBLIC_URL` to the proxy's external address and keep the
published port on loopback (`VOGT_BIND_IP=127.0.0.1`) so the proxy is the
only way in. Forward WebSocket upgrades if the engine is behind the same
proxy — terminal I/O is a WebSocket at `/api/sessions/{id}/attach`. Tell the
proxy not to buffer `/mcp`; it is a streaming transport.

**A front door that remounts the core.** If something publishes the core at
a different address *and* different path prefixes — the session engine does
this, mounting the core's `/api` at `/api/vogt` — set `VOGT_FRONTED=true`
and have the door send `X-Vogt-Public-Url`, `X-Vogt-Api-Path` and
`X-Vogt-Mcp-Path` on each request. `connect` then renders against what the
door says. This is off by default and never inferred, so a stranger who can
reach the core cannot make `connect` render a configuration document against
an address they chose. Keep the core off the published interface in this
shape — reachable on the Compose network only — and give the door its core
token with the bootstrap token file
([`CUSTOMISATION.md`](CUSTOMISATION.md#giving-the-front-door-its-core-token-in-one-deploy)).

Whatever sits in front: `/health/ready` and `/version` must remain plain
HTTP, unauthenticated, and reachable by whatever probes you run.

## 7. Production promotion, Android releases, and data

CI, release, and deployment are deliberately separate: CI proves a commit;
a version tag creates signed, immutable artifacts; deployment selects the
digest a production instance runs. A successful build or published image does
not change production by itself.

The desired state a production instance runs — which digests, which overlays,
which host specifics — is owned by the operator's own deployment repository,
not this one; this tree ships only the estate-neutral base and overlays,
never a turnkey production estate. And the maintainer's own production is one
such private deployment, layering a private overlay on the public base; it is
not a supported drop-in scenario reproducible from this repository alone.

**Two different images exist, and they are now two different packages.** Both
carry the agent CLIs; what separates them is whether they also carry the
maintainer's estate:

| Package | Built by | Visibility | Pod base | Carries | Meant for |
|---|---|---|---|---|---|
| `vogt-stack` | `release.yml` (version tag reachable from `prod`) | public | `lean` | `claude`, `codex` | the signed public artifact — anyone |
| `vogt-stack-estate` | `build.yml` on `dev` / `prod` | private | `full` | the above, plus Flutter/Android SDK, Cadastre MCP, theclawbay | the maintainer's own dev/prod pods |

`vogt-stack` is the one to pin. The estate package is not a "fuller" release —
it is a private deployment's image, carrying integrations that address one
estate's infrastructure and mean nothing outside it.

They were one package until the public AIO shipped, distinguished only by tag
family. That was already a pinning hazard, and publishing the package turned it
into a disclosure one: **GHCR visibility is per package, not per version**, so
making the generic AIO public also published every `dev-`/`prod-` image in it.
No credential was exposed — the build passes none as a build arg and mounts
none — but a stranger being able to pull the estate's pod image is not a
property anybody chose. Splitting the packages makes the boundary something the
registry enforces rather than something a tag convention implies.

What the release digests do *not* carry is Flutter and the Android SDK: those
belong to the `full` pod base, and the signed APK is built by `release.yml`'s
own Android job rather than from inside a pod.

A release also publishes the **core image** (`vogt`) at the same version. It
is the build input the stack image lifts its core from, recorded in the
release manifest so the chain from source to pod is verifiable; it is not a
deployment target (§1.1).

### 7.1 Promote `dev` to production

Three branches, three roles. `dev` is the integration branch: every change
lands there first, the dev pod runs it, and it is the only branch development
targets. `prod` is what production runs. `main` is the transient waypoint
between them — it only ever receives a fast-forward of a validated `dev`
SHA and is then fast-forwarded on to `prod`; no branch is based on it, no
pull request targets it, and no release is cut from it (a version tag must
be reachable from `prod`).

Promotion is two explicit, fast-forward-only pushes. First deploy the
exact `dev-<sha>` images and obtain the verified receipt; the promotion
workflow refuses to promote `dev → main` without that receipt. Run **Actions →
deploy dev**, provide the current full `dev` SHA, type `DEPLOY-DEV`, and keep
the receipt URL and artifact with the change record. The workflow verifies both
signed images, updates the two active digest pins in the operator-managed dev
stack, calls the deployment controller, and runs the live smoke contract. It
uses the configured CI identity to retrieve deployment credentials and
dev-runtime credentials. After deployment, the helper reads the active
runtime token from the deployment-controller stack environment into a `0600`
runner-temp file for live smoke only; it is never printed or committed. No GitHub App,
private key, Forgejo token, version tag, or GitHub Release is required. The
receipt must cover readiness,
authentication, a representative core read/write path, the engine/PWA front
door, and the visible canonical product version/provenance. A failed or stale
receipt is not a promotion approval. `expect_agent_clis` (`codex=<version>,claude-code=<version>`) makes the run fail unless the live pod reports those CLI versions as active — the acceptance for a runtime pin. `repair_environment=inspect` is a read-only dump of what Komodo holds for the stack, keys only.

Then promotion is two explicit, fast-forward-only pushes. Run **Actions →
promote**, choose `dev-to-main`, and type `PROMOTE`. The workflow verifies
that the target is an ancestor of the source, that the source branch is green
(`ci` and `runner-policy`), and that the verified dev deployment receipt
covers the exact source SHA; only then does it fast-forward `main` to that
SHA. Dispatch the same workflow again with `main-to-prod`, and only then
continue below.

GitHub has no fast-forward merge method — every PR merge, rebase merge
included, rewrites the promoted commits' SHAs, which makes the source branch
a non-ancestor of the target and deadlocks the next stage's ancestry gate.
Promotion therefore never goes through a pull request: the
`release-branch-promotion` ruleset blocks deletions and force pushes on
`main` and `prod` outright and requires green `ci` and `runner-policy`
checks on any pushed commit — which a fast-forward of the validated source
SHA carries by construction, and an unvalidated commit never can. The
promotion-policy check fails any stray PR into a release branch early with
the reason. The promoted commit keeps the checks it earned on the
source branch — a fast-forward moves the ref to the identical SHA — and the
workflow dispatches the ref-bound pipelines itself: `build.yml` for the
branch-scoped image tags, plus `ci.yml` on `prod` for the production Android
APK. No token or secret is required beyond the workflow's own. On GitHub
plans that support it, the `promote-main` and `promote-prod` environments
can add a separate reviewer gate before the push.

The Android jobs read their Firebase client configuration from Infisical at
build time. Configure these repository-level GitHub values before a push to
`dev` or a release tag:

- variable `INFISICAL_API_URL`: the self-hosted Infisical API URL (including
  `/api` when that is how the runner's CLI is configured);
- variable `INFISICAL_PROJECT_ID`: the Infisical `apps` project id;
- secrets `INFISICAL_CLIENT_ID` and `INFISICAL_CLIENT_SECRET`: a machine
  identity with read access to the `prod` environment.

The CI Android job fetches `VOGT_FIREBASE_DEV_JSON` and builds
`com.thedancingdeveloper.vogt.dev`; the tagged release job fetches
`VOGT_FIREBASE_PROD_JSON` and builds `com.thedancingdeveloper.vogt`. The fetcher validates
the package entry before Gradle runs and removes the generated
`mobile/android/app/google-services.json` even when the job fails. Pull
requests do not receive the Infisical credentials and continue to use the
sanitized checked-in example.

1. Back up the running instance and copy the resulting backup directory off
   its data volume:

   ```console
   docker compose -f deploy/stack.compose.yml exec vogt \
     vogt backup --reason "pre-production release"
   ```

2. Update `pyproject.toml` to the next product version; the version check keeps
   the Python core, PWA, mobile manifest, engine build metadata, and release
   workflow aligned. Create and push the matching `v<version>` tag. The tagged release publishes
   signed, immutable core and merged-stack image digests. It also builds the
   signed APK when its release prerequisites are configured.
3. A tag also creates one durable GitHub Release after the version check,
   signed images, and signed APK succeed. The Release contains the APK and
   `vogt-release-manifest.json` with the source SHA, image digests,
   provenance, and #377 handoff. Publishing still does not deploy.
4. Configure the `vogt-prod` environment with a narrowly scoped GitHub App
   (`VOGT_DEPLOYMENT_APP_ID`, `VOGT_DEPLOYMENT_APP_PRIVATE_KEY`) and the
   deployment repository variables `VOGT_DEPLOYMENT_REPOSITORY`,
   `VOGT_DEPLOYMENT_REPOSITORY_NAME`, `VOGT_DEPLOYMENT_OWNER`,
   `VOGT_DEPLOYMENT_WORKFLOW`, and `VOGT_DEPLOYMENT_REF`. Dispatch
   `deploy-production.yml`. It verifies ancestry, the signed digest, and then
   sends the release receipt to that workflow. The estate workflow owns the
   desired-state commit, approval, `DeployStack`, live smoke, rollback plan,
   and migration limits, and must upload `vogt-deployment-receipt/receipt.json`,
   validated against [`deploy/vogt-deployment-receipt.schema.json`](../deploy/vogt-deployment-receipt.schema.json).
   The source workflow rejects a green handoff without that receipt. This
   replaces a long-lived broad Komodo credential in the Vogt repository.
5. Check the production front door's `/health/ready`, the engine's `/readyz`
   when deployed, authentication, and one representative read/write workflow.
   Keep the former digest and the pre-release backup until those checks pass.

The mobile shell is a remote WebView. Do **not** issue a new APK for a
server-only or PWA-only release: installed shells load the updated front door.
Create/distribute a new APK only when native code, Capacitor plugins,
permissions, package identity, Firebase configuration, signing identity, or
the baked-in front-door URL changes.

For a production APK, before tagging:

- Set the GitHub secret `VOGT_ANDROID_SERVER_URL` to the exact production
  front-door URL (the same value as `VOGT_PUBLIC_URL`).
- Retain the existing signing identity in the four
  `MYDEVENV2_ANDROID_KEYSTORE_*`, `..._PASSWORD`, `..._KEY_ALIAS`, and
  `..._KEY_PASSWORD` GitHub secrets; replacing the key cannot update devices
  carrying the previous app.
- `mobile/package.json` is checked against the canonical `pyproject.toml`
  version. Gradle derives the Android `versionCode` from it, so each published
  upgrade must be greater than the last.
- Supply the real production `google-services.json` in the release environment
  if Firebase Cloud Messaging is required. The committed example deliberately
  cannot deliver real notifications.

The release workflow runs `apksigner verify` before uploading the
`vogt-android-release-v<version>` artifact. Install it over the prior release
on a real device and verify it loads production before distribution.

### 7.2 What is on disk


One directory, `VOGT_DATA_DIR` (`/var/lib/vogt` in the image, a named
volume in Compose):

- `declared.sqlite3` — the declared store: projects, work, tokens, the audit
  log. This is the thing you cannot regenerate.
- `observed.sqlite3` — what collectors recorded. Regenerable by sweeping,
  though coverage history is lost with it.
- `backups/` — where `vogt backup` writes by default.
- `repos/` — imported repositories, unless `VOGT_IMPORT_ROOT` points
  elsewhere.

Both stores are SQLite in WAL mode, opened per transaction, so a CLI
process, the server and a stdio MCP process can share one directory. Every
write therefore costs a WAL checkpoint and its fsync — around 25 ms on
commodity NVMe. A large estate sweeps slowly for that reason. Do not "fix"
it with `VOGT_SQLITE_SYNCHRONOUS=off` in production: that trades durability
for speed in a product whose declared store is an audit log. The knob exists
for test runs.

### 7.3 Backup and restore

```console
docker compose -f deploy/stack.compose.yml exec vogt \
  vogt backup --reason "nightly"
# → /var/lib/vogt/backups/<timestamp>/ with both stores and manifest.json
```

`backup` uses SQLite's online backup API, so it is consistent while the
server is running. The manifest records the schema version of each store.
In the published stack `VOGT_ENGINE_STATE_DIR` is set and readable, so the
engine's state — session history, push subscriptions, agent tasks — is
copied too and the manifest says so; `/readyz`'s `backup_agreement` check is
what tells you that is still true. Where it is not, the manifest says
`not configured`, so a core-only backup never pretends to be more. Copy the backup directory off
the host; a backup on the volume it protects is not a backup.

```console
docker compose -f deploy/stack.compose.yml exec vogt \
  vogt restore --source /var/lib/vogt/backups/<timestamp> \
  --confirm --reason "restore after volume loss"
```

`restore` verifies the manifest before touching anything and refuses a
backup whose schema is *ahead* of the running build. Stop the server's
traffic first if you can; restore from the container that owns the data
directory, never from a front-door container that merely has a `vogt`
binary.

### 7.4 Upgrade

1. Take a backup (§7.3).
2. Change `VOGT_STACK_IMAGE` **and** `VOGT_VOICE_IMAGE` in `deploy/.env` to the
   new release's digests (or tags) — the pair is versioned together (§2.2).
3. `docker compose -f deploy/stack.compose.yml up -d --wait`.
4. Watch `/health/ready`: it answers 503 — naming the store and both
   schema numbers — until `init` and the startup migration complete,
   then 200 with the applied and expected schema versions. Migrations
   are forward-only and run under a lock, so two containers starting at once
   cannot race.

Read [`SCHEMA.md`](SCHEMA.md) before a major version: it lists every
migration and what each changes.

### 7.5 Rollback

A rollback is the two digest lines reverted plus `up -d` — **unless the upgrade
applied a migration**. Migrations are forward-only: an older build against a
newer store keeps answering `ready` (deliberately, so a deliberate rollback
does not look like a broken container), but `vogt migrate` refuses the
store, naming the migration, and operations that touch the changed tables
fail. Rolling back across a schema change means restoring the backup from
step 1, not just running the older image.

## 8. Public demo image

The `demo-runtime` target in `engine/Dockerfile` builds a separate, static-only
artifact from `dev`.
It compiles the Solid PWA once, records each asset hash and the exact source
SHA in `demo-build.json`, verifies them, and only then adds
`demo-manifest.json` plus the simulated GUI document. The runtime is a small
read-only Node static server. It has no Python core, Rust engine, PTY,
subprocess route, workspace mount, upstream proxy or deploy credential.
Demo augmentation also adds `mobile-demo.html`; that page frames the same PWA
at phone width to demonstrate the implemented Capacitor WebView UI without
shipping a second frontend.

The `demo-image` job in `build.yml` runs only for `dev`, smoke-tests that APIs
are refused, emits an SBOM and signs the digest. It **does not deploy**.
Deployment follows the same NFR-D10 path as every other Vogt image: pin the
reported `ghcr.io/thedancingdeveloper-org/vogt-demo@sha256:…` reference in the
operator's deployment stack and let its approved workflow apply it. The repository-local
[`deploy/demo.overlay.yml`](../deploy/demo.overlay.yml) documents the hardened
runtime and safe allocation defaults; it is not an alternate deployment path.

The same deployed origin serves the mobile showcase at `/mobile-demo.html`.
To give it a separate mobile-first hostname, run a second Compose project from
the **same signed digest** and layer
[`deploy/mobile-demo.overlay.yml`](../deploy/mobile-demo.overlay.yml) over the
demo service. That overlay changes only the root document; `/index.html`
remains the real PWA loaded inside the phone frame, so the showcase cannot
recurse into itself. Publishing either entry point still deploys nothing; the
operator-owned digest update remains the only movement step.

To build and prove the artifact locally:

```console
docker build -f engine/Dockerfile --target demo-runtime \
  --build-arg VOGT_SOURCE_REF=dev \
  --build-arg VOGT_SOURCE_SHA="$(git rev-parse HEAD)" .
```

The public origin should expose only port 8910 through the operator's chosen
private/TLS binding. It needs no token or persisted volume. A stale source SHA
or asset hash makes demo augmentation (and browser selection) fail rather than
claiming parity.

## 9. Troubleshooting

**`/health/ready` returns 503 with "run `vogt init` first".** The container
command is not the base's `vogt init && exec vogt serve`, or the data
directory is not the one `init` wrote to. Check `VOGT_DATA_DIR` and the
volume mount; do not run `init` in a container that is not the data owner —
that creates a second, empty instance.

**The server never starts and the log shows only `init` ran.** The Compose
`command:` was written as a folded string. Compose word-splits a string
command and truncates it at `&&`; keep the single-element list form the
base uses.

**`VOGT_PUBLIC_URL` error at `up`.** It is `:?`-gated in the base. Put it in
`deploy/.env`, or pass `--env-file`.

**Permission denied on `/var/lib/vogt` or at `project import`.** The uid in
`user:` cannot write the volume or the bind mount. A named volume works at
any uid with gid 0; a bind mount carries the host's ownership, so set
`VOGT_UID` to the owner of the mounted tree.

**`connect` reports no public URL, or the wrong one.** `VOGT_PUBLIC_URL` is
unset, or the instance is behind a remounting front door without
`VOGT_FRONTED=true`. A wrong URL and an unreachable one look identical from a
client, which is why the value is never guessed.

**GitHub data is empty but nothing reports an error.** The forge adapter is
off because `VOGT_GITHUB_TOKEN_FILE` is unset or unreadable — absence is
*not collected*, silent by design. Check `vogt coverage`, which says what
has looked at what. Note that a `read_only` container cannot take a Compose
secret sourced from an environment variable (Docker must write it into the
container); use a `file:` secret or a read-only bind mount.

**`session.*` says no engine configured.** `VOGT_ENGINE_URL` and
`VOGT_ENGINE_TOKEN_FILE` are unset on the core. The engine is optional; this
is the honest answer, not a fault.

**The engine starts but every Vogt route answers 502 or 503.** `VOGT_CORE_URL`
points at nothing listening (a non-loopback URL means the engine will *not*
start a core of its own), or the core token is missing — the error names the
token whose pairing is absent. `/readyz` on the engine reports the core's
state in full.

**Assistant tab missing, or the engine refuses to start mentioning
`assistant_base_url`.** No `ENGINE_ASSISTANT_API_KEY` means the assistant is
off and the tab is hidden. A key *without* `ENGINE_ASSISTANT_BASE_URL` is a
configuration error on purpose: the endpoint a key is sent to is an
exposure decision and is never defaulted.

**Fetches fail intermittently through a VPN or overlay network.** Some
overlay networks and HTTP/3-capable edges interact badly with long-lived
streaming responses. Try forcing HTTP/1.1 or HTTP/2 at the proxy for `/mcp`
and WebSocket paths before suspecting Vogt.

**`vogt: not found` inside the container.** The virtualenv at
`/opt/vogt/.venv/bin` is on `PATH` for `docker exec <container> vogt …` but
not necessarily for a login shell (`sh -lc`). Use the direct form.

Operator-local notes about a particular host belong in the git-ignored
`docs/local/`, not in this file.
