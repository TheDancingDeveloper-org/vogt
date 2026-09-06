# Customising Vogt

Vogt ships as a generic product: one published image (`vogt-stack`), one
Compose base ([`deploy/stack.compose.yml`](../deploy/stack.compose.yml)), and
a configuration schema that decides everything an operator is allowed to
decide. It is also meant to be customised heavily — the expected way to run
it in an estate of your own is to fork this repository and layer your estate
on top of that image. This document names the supported extension points, so
that a large customisation is a documented deployment rather than a divergent
fork.

The rule underneath all of it: **the base is never edited.** If you find
yourself changing `deploy/stack.compose.yml` or `engine/Dockerfile` to make
your deployment work, that is a gap in this document — please raise it.

## Three layers

| Layer | Use it for | Cost of an upgrade |
|---|---|---|
| **Configuration** | Anything the schema already decides | None. New image, same settings. |
| **Compose overlay** | Extra services, mounts, networks, ports, secrets | None. Your overlay states only differences. |
| **Image extension** | Extra binaries, certificates, collectors, your estate's integrations | A rebuild against the new base digest. |

Reach for them in that order. Most customisations stop at the first; an
estate that needs its own tools inside sessions ends at the third, and
[Extending the stack image](#extending-the-stack-image) is the worked pattern.

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
docker compose -f deploy/stack.compose.yml -f my-deployment.yml up -d
```

The repository ships a small example of this:
[`deploy/vogt.build.yml`](../deploy/vogt.build.yml) — for the contributor
stack — does nothing but swap a published image for a build from the
checkout.

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
read it. Mount it under the pod's `Working` tree, which is both the core's
import root and the root a session opens in — the two must agree, and
`/readyz` reports `workspace_agreement` false when they do not:

```yaml
services:
  vogt:
    volumes:
      - /srv/repos:/home/sprooty/Working/repos:rw
```

The uid matters. The pod runs as a fixed user, `sprooty`, uid 1000, and a
bind mount carries the *host's* ownership, not the image's. A directory that
uid cannot read fails at `project import` rather than at startup, and one it
cannot write fails the first time a session tries to commit — both slow ways
to find out. Own the host directory by uid 1000, or extend the image and
change the user there.

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

- `ENGINE_PORT` — the bind fails otherwise, which at least tells you.
- `ENGINE_PUBLIC_URL` — each instance has its own address, and this one is
  never inferred.
- The core token file, if you keep it beside the Compose file — give each
  instance its own.
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

### Voice (STT/TTS)

Voice works out of the box. The supported all-in-one stack bundles a
first-party `voice` sidecar — the Rust `vogt-voice` image, carrying native
Whisper and Piper and a small, permissively-licensed default model set
(Whisper `base.en`, a public-domain Piper English voice) — and
[`deploy/stack.compose.yml`](../deploy/stack.compose.yml) wires the engine to
it over the Compose network. A fresh `docker compose -f deploy/stack.compose.yml
up -d --wait` transcribes the microphone and speaks replies with no account and
no extra file. The sidecar is never published to the host; the engine stays the
only front door.

The engine speaks the standard OpenAI audio interface —
`POST /v1/audio/transcriptions` for speech-to-text and `POST /v1/audio/speech`
for text-to-speech — and its base URLs are an ordered fallback list, so none of
this locks you to the sidecar or to any one vendor.

**Turn it off.** The sidecar sits behind a Compose profile that
`deploy/stack.env.example` enables. Clear `COMPOSE_PROFILES` in `deploy/.env`
and the sidecar does not start; the voice controls stay present but inert.

**Point at another provider.** Set the base URLs (and a key, model, voice and
format) in `deploy/.env` to OpenAI, Groq, or any OpenAI-compatible speech
endpoint. A cloud provider serves `mp3`, so set the format back to `mp3` when
you repoint TTS; you can then also clear `COMPOSE_PROFILES` so the local sidecar
does not run:

```dotenv
ENGINE_ASSISTANT_STT_BASE_URLS=https://api.openai.com/v1
ENGINE_ASSISTANT_STT_API_KEY=sk-...
ENGINE_ASSISTANT_STT_MODEL=whisper-1
ENGINE_ASSISTANT_TTS_BASE_URLS=https://api.openai.com/v1
ENGINE_ASSISTANT_TTS_API_KEY=sk-...
ENGINE_ASSISTANT_TTS_MODEL=tts-1
ENGINE_ASSISTANT_TTS_VOICE=nova
ENGINE_ASSISTANT_TTS_FORMAT=mp3
```

The `ENGINE_ASSISTANT_TTS_FORMAT` value is what the engine asks the TTS
endpoint for and defaults to `wav` in the stack, because the sidecar's Piper
backend serves only `wav`; the engine streams the upstream content type straight
through, so either plays in the PWA.

**Bring your own models.** The sidecar's default weights are baked in, but its
`/health` still gates on whatever model paths it is given, so an operator can
point it at models of their own — build an image that starts `FROM` the
published sidecar and overrides `VOGT_VOICE_STT_MODEL_PATH` /
`VOGT_VOICE_TTS_MODEL_CONFIG_PATH`, or set `VOGT_VOICE_STT_COMMAND` /
`VOGT_VOICE_TTS_COMMAND` to run inference executables of your own. The Piper
JSON model configuration must sit beside its ONNX file under the same stem (for
example `voice.onnx.json` beside `voice.onnx`). See
[`../voice/README.md`](../voice/README.md) for the native model and request
details.

### Giving the front door its core token in one deploy

A fronted deployment needs a token the front door presents to the core, so
audit rows name the actor who acted rather than "the proxy". The core is what
validates it — which, without a bootstrap path, would mean it could only be
*minted* by a running core, making a first deploy: start up, watch
`/api/vogt` answer 401, exec into the core, mint a token, paste it into your
configuration, deploy again.
The second deploy is not free either: it restarts the pod and takes every
open terminal session with it.

Choose the value instead, exactly as you already choose the engine's session
token:

```bash
openssl rand -hex 32 > core-token
```

The published stack does this for you: `deploy/stack.compose.yml` mounts
`deploy/vogt-core-token` as a secret both halves read, so the file above is
the only step. What follows is the same mechanism spelled out for a
deployment that runs the halves in separate containers — mount the file into
**both**. The front door presents it; the core adopts it at `init`:

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

Not a concern for the published stack — one container holds both halves, and
`docker compose exec vogt vogt …` reaches the data. Worth knowing before you
run an administrative command against a deployment of your own that splits
them into two services, because the wrong container fails in a way that
reads like a broken instance rather than like a typo.

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

This is the layer an estate lives in, and
[Extending the stack image](#extending-the-stack-image) below is the whole
pattern: a Dockerfile a few lines long that starts `FROM` the published
`vogt-stack` digest and adds what your sessions need. Keep it in your fork,
build it in your CI, deploy it by digest.

The core image (`ghcr.io/thedancingdeveloper-org/vogt:0.5.4`) is also built
to be a base, and the contributor stack extends it the same way — `USER
root`, install, `USER 1000:0`, keeping `ENTRYPOINT ["vogt"]` and the
`root:0`-owned data directory. It is a build input to the stack image rather
than something to deploy on its own, so extend it only when you are changing
what the *core* carries.

## Extending the stack image

A worked, buildable version of everything below lives at
[`deploy/examples/custom-stack/`](../deploy/examples/custom-stack/) — a
Dockerfile, an overlay, and a lifecycle hook you can build and boot as-is. It is
the exact shape the private `vogt-dev` and `vogt-prod` derivatives take: they
differ from it only in *which* tools and integrations they add, never in the
mechanism. CI builds and boots it with no private integrations, so the contract
stays honest.

**Derive from the published digest.** An extending image starts `FROM` the
signed release, by digest, not by tag:

```dockerfile
FROM ghcr.io/thedancingdeveloper-org/vogt-stack@sha256:<digest>
USER root
RUN apt-get update && apt-get install --no-install-recommends -y <your tool> \
    && rm -rf /var/lib/apt/lists/*
USER sprooty
```

The image ends as `USER sprooty` (uid 1000). Take root back to install and hand
it straight back — an image that forgets the second half runs your pod as root,
a different container from the one whose digest you pinned.

**Image-owned versus persisted versus deployment-owned.** Three kinds of path
live in a running stack, and only one of them your `Dockerfile` owns:

- **Image-owned** — what the `Dockerfile` writes: a tool in `/usr/local` or
  `/usr/bin`. A new release replaces these wholesale.
- **Persisted** — the named volumes the base declares: `engine-home`
  (`/home/sprooty`, the writable pod home and the `Working` tree) and
  `vogt-data` (`/var/lib/vogt`, the core's SQLite stores and backups). These
  carry your data across upgrades.
- **Deployment-owned** — what your overlay mounts: lifecycle hooks at
  `/run/vogt/hooks:ro`, secrets as Compose secret/config mounts. The image's
  hook runner never copies or discovers scripts from the image, so hooks are
  mounted, never baked in.

**The shadowing trap.** A named volume mounted over a path the image populated
*hides* the image's copy. `engine-home` covers `/home/sprooty`, so a file your
`Dockerfile` writes inside `$HOME` is invisible once the volume mounts — the
volume wins. Put tools in `/usr/local` (outside any volume); let the deployment
own what lives under `$HOME`.

**The loopback-core invariant survives extension.** The AIO runs its own core on
loopback and the engine is the only front door (NFR-D11). An extension adds
tools and mounts; it does not publish the core's port or point `VOGT_CORE_URL`
at anything but loopback. The entrypoint refuses to start if it does.

**Prove it at build.** Build the extended image in your own CI and boot it there
before you deploy it — the worked example's CI job is the template. A tool that
fails to install, or a hook that fails its contract, is a broken build, not a
surprise in production.

**Upgrades, migrations, and rollback.** Upgrading is a digest change in your
fork and a rebuild — bump the `FROM` stack digest and the matching
`VOGT_VOICE_IMAGE` (they are one release pair; see
[`DEPLOYMENT.md` §7.4](DEPLOYMENT.md#74-upgrade)), rebuild, redeploy. The core
runs its forward-only migrations against `vogt-data` on start. **Rollback is not
symmetric**: once a newer image has migrated the data, an older image may refuse
that schema, so roll back only to a release whose schema the data still matches,
or restore `vogt-data` from a pre-upgrade backup. See
[`DEPLOYMENT.md` §7.5](DEPLOYMENT.md#75-rollback).

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

The largest customisation this repository knows about is the maintainer's
own estate: the public `vogt-stack` digest, plus a layer that adds the
estate's integrations (a tailnet, a secrets broker, an infrastructure MCP
server), plus a private Compose overlay carrying host mounts and addresses.
It is built entirely from the extension points above — image extension,
overlay, configuration — and nothing else. Neither the layer nor the overlay
is tracked here: a deployment tied to one operator's paths and addresses
does not belong in a public tree, and lives in the operator's private
repository instead.

That is the shape a fork of your own should take. Every estate-specific value
is an environment value, a mount, or a line in your own Dockerfile — never a
default baked into a file a stranger clones — so a new release of the public
image is a digest change in your fork and a rebuild, not a merge.

The two-container contributor stack ([`deploy/vogt.compose.yml`](../deploy/vogt.compose.yml)
plus [`deploy/engine.overlay.yml`](../deploy/engine.overlay.yml)) is a second
worked example, of the *overlay* layer: the engine overlay adds one service
and some configuration in front of the unmodified core image, and
`tests/test_public_delivery.py` pins that it carries no host paths, no
tailnet and no maintainer integrations. Read it as a diff to see what an
overlay is allowed to say; run it when you are changing the engine.
