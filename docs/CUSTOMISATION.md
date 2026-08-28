# Customising Vogt

Vogt ships as a generic product: one Python core image, one Compose base, and
a configuration schema that decides everything an operator is allowed to
decide. It is also meant to be customised heavily. This document names the
supported extension points, so that a large customisation is a documented
deployment rather than a fork.

The rule underneath all of it: **the base is never edited.** If you find
yourself changing [`deploy/vogt.compose.yml`](../deploy/vogt.compose.yml) or
the root `Dockerfile` to make your deployment work, that is a gap in this
document — please raise it.

## Three layers

| Layer | Use it for | Cost of an upgrade |
|---|---|---|
| **Configuration** | Anything the schema already decides | None. New image, same settings. |
| **Compose overlay** | Extra services, mounts, networks, ports, secrets | None. Your overlay states only differences. |
| **Image extension** | Extra binaries, certificates, collectors | A rebuild against the new base tag. |

Reach for them in that order. Most customisations stop at the first.

## Layer 1 — configuration

Every setting lives in one schema, `src/vogt/config.py`.
[`CONFIG.md`](CONFIG.md) and `config.example.toml` are *generated* from it —
never hand-edit them; run `uv run python scripts/gen_config_docs.py` and
commit the result.

Precedence, highest first:

1. explicit command-line arguments
2. `VOGT_*` environment variables
3. the TOML file named by `VOGT_CONFIG_FILE`
4. schema defaults

Each setting is classified by what it decides, and the class tells you
whether it can carry a default:

- **exposure** — an address, a port, a URL a client will trust. Never
  defaulted anywhere, in code, images, docs or examples. You must state it.
- **allocation** — a path or a slot on a host you own. Always defaulted, so a
  deployment never fails merely because the example did not know your
  filesystem layout.
- **behaviour** — tuning that decides neither. Unconstrained.

`tests/test_config.py` asserts the first two rules against the live schema, so
this is a property of the product rather than a convention.

### Settings worth knowing about

**Storage and identity.** `data_dir` holds both SQLite stores and the
backups; one instance per directory. `import_root` is where imported
repositories are cloned, defaulting to `<data_dir>/repos` — see
[Observing an estate](#observing-an-estate-on-a-host-path) before pointing it
elsewhere.

**What the contract means.** `contract_required_files`,
`contract_required_dirs` and `contract_required_meta` define the project
contract `contract check` reports against. This is a value you read, never a
barrier you pass — changing it changes what is reported and gates nothing. If
you change the rules and leave `contract_version` alone, Vogt appends a digest
of your rules to the version, so a recorded status can never claim to be the
stock `v1` when it is not.

**What counts as work.** `marker_promotion_patterns` decides which source
markers enter backlog and bug views; the default is `TODO(vogt):`. Every other
marker is still observed, still queryable, still counted — it just does not
claim to be work. Widening this is how you drown the ranked view.
`marker_file_extensions` decides which files the marker collector reads,
because which extensions hold source is your estate's business, not Vogt's.

**Collection cadence.** `sweep_interval_seconds`, `verify_horizon_hours` and
`retention_days`. Collection scope is always the projects you registered —
there is no setting that makes Vogt crawl, and that is deliberate.

## Layer 2 — Compose overlays

The base file is designed to be layered on. Compose merges files
left-to-right, so an overlay states only its difference:

```console
docker compose -f deploy/vogt.compose.yml -f my-deployment.yml up -d
```

The repository ships the smallest possible example of this,
[`deploy/vogt.build.yml`](../deploy/vogt.build.yml), which does nothing but
swap the published image for a build from the checkout.

An overlay is the right place for extra services, host mounts, a different
network, additional secrets, TLS material, or a reverse proxy. Keep your
overlay in your own repository beside the rest of your deployment
configuration — publishing an image and moving a deployment stay separate
acts, and a digest or configuration change is what moves one.

### Deployment-owned lifecycle hooks

Mount an executable bundle from your deployment repository at
`/run/vogt/hooks:ro`, using `pre-start.d`, `post-start.d`, and/or
`post-health.d` directories. The image's generic runner provides phase,
working-directory, hook-root, and first-start environment variables; it never
copies or discovers scripts from the image. Hooks run in lexical order, fail
the service on non-zero status, and must be safe to rerun. Set
`VOGT_HOOKS_REQUIRED=true` for a required bundle and pass credentials through
Compose secret/config mounts rather than the image or Git. The public base
needs no hook mount and remains functional unchanged.

### Observing an estate on a host path

Vogt's collectors read the repositories you register, so a deployment that
observes a host directory has to mount it into the container *and* be able to
read it:

```yaml
services:
  vogt:
    user: "1000:0"          # the uid that owns the files being observed
    environment:
      VOGT_IMPORT_ROOT: /workspace/repos
    volumes:
      - /srv/repos:/workspace:rw
```

The uid matters. The image runs as any uid provided the gid is 0 — that is
why `/var/lib/vogt` is owned by group 0 and group-writable — but a bind mount
or a fresh named volume carries the *host's* ownership, not the image's. A
directory the container user cannot write to will fail at `project import`
rather than at startup, which is a slow way to find out.

### Running a second instance on the same host

Two instances on one host — a production one beside a staging one, say — is
supported, and it is the case where every default that names a host-wide
thing becomes a collision. Compose covers some of it for you and not the
rest.

What Compose handles: the named volume in the base file is
*project-scoped*, so two deployments started under different project names
(`docker compose -p vogt-staging …`) already get separate volumes and separate
networks. Use a distinct project name for each instance and most of the
problem does not arise.

What you must still change per instance:

- `VOGT_PORT` — the bind fails otherwise, which at least tells you.
- `VOGT_PUBLIC_URL` — each instance has its own address, and this one is
  never inferred.
- Any host path an overlay of yours bind-mounts. Two instances writing one
  workspace is not an error, it is just wrong.
- Any explicit `name:` you have put on a volume or network in your own
  overlay. Naming them is what opts you *out* of the project scoping above,
  and a shared data volume means two writers on one SQLite database — the
  one failure here that does not stop the deploy and shows up later looking
  like corruption.

If your overlay pins a subnet, do not pick it by incrementing from the
instance next door; enumerate what is actually in use first.
[`DEPLOYMENT.md`](DEPLOYMENT.md#24-a-second-instance-on-the-same-host) has
the worked command line.

### Putting your own front door in front of Vogt

Vogt supports running behind something that publishes it at a different
address and different mount points. Set `fronted = true`, and your front door
states the identity per request with three headers:

- `X-Vogt-Public-Url`
- `X-Vogt-Api-Path`
- `X-Vogt-Mcp-Path`

`connect` and `/connection-info` then render client configuration against what
the door says, because the door is the only thing that knows where clients
arrive. This is off by default and never inferred: an instance that has not
been told it is fronted ignores those headers entirely, so nobody who can
reach it can make `connect` render a configuration document against an address
they chose.

The two-service shape this enables — your front door on the published port,
the core reachable only on the Compose network — is a supported topology, not
a workaround.

### Self-hosted voice (STT/TTS) as an optional overlay

The engine's voice assistant speaks the standard OpenAI audio interface —
`POST /v1/audio/transcriptions` for speech-to-text and `POST /v1/audio/speech`
for text-to-speech — configured entirely by environment. On the public stack
both point at nothing, so a fresh install shows the voice controls but they are
inert until you name an audio provider.

You have two ways to make them work, and neither locks you to a vendor.

**Point at any provider.** Set the base URLs (and a key, model and voice) in
`deploy/.env` to OpenAI, Groq, or any OpenAI-compatible speech endpoint:

```dotenv
ENGINE_ASSISTANT_STT_BASE_URLS=https://api.openai.com/v1
ENGINE_ASSISTANT_STT_API_KEY=sk-...
ENGINE_ASSISTANT_STT_MODEL=whisper-1
ENGINE_ASSISTANT_TTS_BASE_URLS=https://api.openai.com/v1
ENGINE_ASSISTANT_TTS_API_KEY=sk-...
ENGINE_ASSISTANT_TTS_MODEL=tts-1
ENGINE_ASSISTANT_TTS_VOICE=nova
```

**Or run it locally with no account.** Layer `deploy/voice.overlay.yml`. It
adds two small, CPU-only, OpenAI-compatible containers — `whisper`
([speaches](https://github.com/speaches-ai/speaches), faster-whisper) for STT
and `tts` ([openedai-speech](https://github.com/matatonic/openedai-speech),
Piper voices) for TTS — on the same Compose network as the engine, and points
the engine's speech base URLs at them:

```bash
docker compose \
  -f deploy/vogt.compose.yml \
  -f deploy/engine.overlay.yml \
  -f deploy/voice.overlay.yml \
  up --build -d
```

Notes:

- Neither service is published to the host; the engine reaches them by service
  name over the Compose network (`http://whisper:8000/v1`,
  `http://tts:8000/v1`).
- Model weights persist in named volumes (`whisper-cache`, `tts-voices`), so
  the download happens once. The `whisper` container pre-fetches its
  faster-whisper model at boot before it serves, so the first utterance works
  without a manual download step; the first start therefore takes a minute or
  two longer while the model lands.
- The defaults are the ids these two images actually serve —
  `Systran/faster-whisper-small` for STT, `tts-1` + voice `nova` for TTS. Every
  one stays a `${VAR:-default}`, so the same overlay can front a different
  local model or a hosted provider by setting the matching `ENGINE_ASSISTANT_*`
  variable in `deploy/.env`.

This is an ordinary Layer 2 overlay: it states only the speech wiring and the
two services, and it composes with any other overlay you already layer.

For a Vogt-owned runtime, use `deploy/voice.firstparty.overlay.yml` instead:
it builds the Rust `vogt-voice` image from `voice/Dockerfile`, mounts a
read-only operator model directory, and points both audio halves at the one
sidecar. Set these values in `deploy/.env` before starting it:

```dotenv
VOGT_VOICE_MODEL_DIR=/srv/vogt/voice-models
VOGT_VOICE_STT_MODEL_FILE=ggml-base.en.bin
VOGT_VOICE_TTS_MODEL_CONFIG_FILE=en_US-lessac-medium.onnx.json
```

The Piper JSON file must have its neighboring ONNX file under the same stem
(for example `voice.onnx.json` beside `voice.onnx`). The native sidecar accepts
WAV, WebM/Opus, and Ogg audio for STT and returns WAV for TTS; it loads both
models at startup and does not download weights. Its `/health` probe remains
`503` until a required
model is valid. See [`../voice/README.md`](../voice/README.md) for the native
model and request details.

### Giving the front door its core token in one deploy

A fronted deployment needs a token the front door presents to the core, so
audit rows name the actor who acted rather than "the proxy". The core is what
validates it — which used to mean it could only be *minted* by a running
core, and a first deploy went: start up, watch `/api/vogt` answer 401, exec
into the core, mint a token, paste it into your configuration, deploy again.
The second deploy is not free either: it restarts the pod and takes every
open terminal session with it.

Choose the value instead, exactly as you already choose the engine's session
token:

```bash
openssl rand -hex 32 > core-token
```

Mount that file into **both** containers. The front door presents it; the
core adopts it at `init`:

```yaml
services:
  vogt:                       # the core
    environment:
      VOGT_BOOTSTRAP_CORE_TOKEN_FILE: /run/secrets/core_token
      # Optional. Everything in the pod runs as one uid and can read the
      # file, so this scope is the pod's blast radius — keep it narrow.
      VOGT_BOOTSTRAP_CORE_TOKEN_SCOPES: read,work.write,project.write
      VOGT_BOOTSTRAP_CORE_TOKEN_ACTOR: agent:my-front-door
    secrets: [core_token]

  engine:                     # your front door
    environment:
      VOGT_CORE_TOKEN_FILE: /run/secrets/core_token
    secrets: [core_token]
```

Adoption is idempotent: `init` runs on every container start, and a boot that
finds the secret already present writes nothing. Rotating means putting a new
value in the file — the old token stays valid until you revoke it, so the two
acts are separate on purpose.

Two ways this refuses rather than degrades, both deliberate. A secret shorter
than 24 characters is rejected, so a placeholder never becomes a working
credential; and an unknown scope fails startup rather than being ignored,
because the alternative is a deployment that believes it supplied a
credential and silently did not. An *absent or empty* file is neither of
those — it simply means "not configured", and you get the mint-then-configure
path exactly as before.

### In a split deployment, the CLI and the database are in different containers

Worth knowing before you run an administrative command against a two-service
deployment, because the wrong container fails in a way that reads like a
broken instance rather than like a typo.

The core owns the data directory. The front door does not — it proxies to
the core over the network and never opens the database. If your front-door
image also happens to carry a `vogt` binary, that binary in that container
is talking to *nothing*:

```console
$ docker exec <core-container> vogt status
instance_id: ins_...
data_dir: /var/lib/vogt
counts:
  projects: 50

$ docker exec <front-door-container> vogt status
error: not_initialized: no Vogt instance in /var/lib/vogt — run `vogt init` first
```

Both containers can show a `/var/lib/vogt`, which is what makes this
confusing: the core's is the real named volume, and the front door's is
whatever its image declares — frequently an empty anonymous volume that
Docker creates fresh on each deploy.

So **administrative commands go to the core**: `project import`,
`project register`, `token issue`, `status`. Anything you would run to
change what the instance knows belongs where the instance's state is. The
tempting `vogt init` that the error message suggests is exactly the wrong
response — it would initialise a second, empty instance in the container
that should not have one.

One wrinkle if you go looking: the core's CLI may be inside a virtualenv
(`/opt/vogt/.venv/bin`) that is on `PATH` for a plain `docker exec` but not
for a *login* shell. `docker exec core vogt status` finds it;
`docker exec core sh -lc 'vogt status'` may not, and reports
`vogt: not found` — a missing binary that is not missing.

## Layer 3 — extending the image

The published image is built to be a base:

- `ENTRYPOINT ["vogt"]` with `CMD ["--help"]`, so it has no default listen
  address to inherit;
- runs as any uid with gid 0;
- `/var/lib/vogt` owned by `root:0`, mode `0770`;
- the virtualenv at `/opt/vogt/.venv`, already on `PATH`;
- `git` installed, because `project import` and the git collector shell out
  to it.

```dockerfile
FROM ghcr.io/thedancingdeveloper-org/vogt:0.3.0
USER root
RUN apt-get update \
    && apt-get install -y --no-install-recommends ripgrep \
    && rm -rf /var/lib/apt/lists/*
USER 1000:0
```

Keep the uid/gid contract and the data directory's ownership; everything else
in the image is yours to add to. Preserve `ENTRYPOINT` unless you have a
reason — the absence of a default host and port is a safety property, not an
oversight.

## Optional integrations

Each of these is absent by default, and its absence is reported honestly
rather than being fatal. A Vogt installation with all of them off is a
complete, supported product.

| Integration | Turned on by | What its absence means |
|---|---|---|
| **GitHub** | `github_token_file` | "GitHub was not collected", never "there are no GitHub subjects" |
| **MCP** | running `vogt-mcp`, or `/mcp` on a running server | Agents cannot connect; nothing else changes |
| **Remote MCP** | `vogt-mcp-remote` with `VOGT_URL` and `VOGT_TOKEN_FILE` | As above |
| **Session engine** | `engine_url`, `engine_token_file`, `engine_state_dir` | `session.*` operations report that no engine is configured; nothing else is affected |
| **Engine integrations** (voice assistant provider, speech, push, GUI stream, external MCP servers) | the session engine's own `ENGINE_*` configuration | Documented in [`DEPLOYMENT.md` §5.2](DEPLOYMENT.md#52-engine-integrations) and [`ENGINE.md`](ENGINE.md) §9 — each is absent by default and says so |

Tokens are always given as a *file path*, never as a value in the
environment: a token in the environment is a token in every `docker inspect`.

## What is not an extension point

- **The generated configuration reference.** `CONFIG.md` and
  `config.example.toml` come from the schema. Editing them to hide a schema
  change is the one thing CI will not let you do.
- **The operation registry.** The CLI, REST and MCP surfaces are generated
  from one registry, and parity between them is asserted by tests. Adding a
  capability to one adapter alone is not a customisation, it is a bug.
- **The audited write path.** Every write requires a principal and a reason.
  There is no setting that turns this off.
- **Collection scope.** Vogt observes the projects you registered. Nothing
  widens that to a filesystem crawl.

## A worked example

The largest customisation this repository knows about is a two-service
deployment: the session engine as the front door on the published port, the
core detached behind it on the Compose network, a shared workspace mount, a
chosen uid, and a bootstrapped core token. It is built entirely from the
extension points above — front door, detached core, host mounts, uid
selection, optional integrations — and is described in
[`DEPLOYMENT.md`](DEPLOYMENT.md) §3.2 and §6 rather than as the way Vogt is
meant to be run.

You can read it as a diff. [`deploy/engine.overlay.yml`](../deploy/engine.overlay.yml)
is that deployment expressed against this same base:

```console
docker compose -f deploy/vogt.compose.yml -f deploy/engine.overlay.yml up --build -d
```

It adds one service and some configuration. It does not rebuild, repin, or
restate the core — that is the published image, unmodified, which is what
makes "a customised deployment is the public image plus configuration" a
claim you can check rather than one you have to believe
(`tests/test_public_delivery.py` checks it). No engine image is published, so
the overlay always builds one from this checkout; it carries no host paths, no
tailnet and no maintainer integrations, so it runs on any host unchanged.

The maintainer's own estate layers its host mounts, tailnet, and secret
integrations on top of this same base. That overlay is not tracked in this
repository (#204) — a deployment tied to one operator's paths and addresses
does not belong in a public tree — and lives in the operator's private ops
repository instead. Treat `engine.overlay.yml` as the pattern: every
estate-specific value it would add is an environment value or a mount an
operator supplies, never a default baked into a file a stranger clones.

If your deployment needs something none of these layers reach, that is worth
an issue. The generic base is only generic if the customisations people
actually need are supported ones. Notes about a particular host of your own
belong in the git-ignored `docs/local/`.
