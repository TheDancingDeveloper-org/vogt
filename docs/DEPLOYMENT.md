# Deploying Vogt

How to run the published Vogt stack somewhere that is not your laptop: what
the images are, what must be configured before the stack starts, how to put
it behind TLS, and how to keep the data safe across upgrades.

This is the operator's document. [`GETTING_STARTED.md`](GETTING_STARTED.md)
covers a first local run; [`CONFIG.md`](CONFIG.md) is the generated reference
for every core setting; [`ENGINE.md`](ENGINE.md) is the session engine in
full; [`CUSTOMISATION.md`](CUSTOMISATION.md) is how to layer your own tools
and integrations onto the published image.

## 1. What you are deploying

Vogt ships as one stack: the `vogt-stack` image plus the `vogt-voice`
sidecar, released as a pair and run by one Compose file,
[`deploy/stack.compose.yml`](../deploy/stack.compose.yml).

- **`ghcr.io/thedancingdeveloper-org/vogt-stack`** carries the Python core,
  the Rust session engine, the Solid PWA and the `claude` and `codex` agent
  CLIs. It publishes one port, 8910. Inside the container the engine is the
  front door and the core listens on loopback only:

  ```
  vogt-engine  (:8910, published)
    ├── /               the PWA
    ├── /api/...        sessions, terminals, files, git, agent tasks, push
    ├── /api/vogt/...   proxied to the core, with the core token injected
    ├── /mcp            proxied to the core
    └── /healthz, /readyz
  vogt serve   (:8000, loopback inside the container, never published)
    ├── /api/...        REST (OpenAPI at /openapi.json)
    ├── /mcp            MCP streamable HTTP transport
    └── /health/live, /health/ready, /version
  ```

- **`ghcr.io/thedancingdeveloper-org/vogt-voice`** is the speech sidecar:
  native Whisper and Piper with a small baked model set, reached by the engine
  over the Compose network as `http://voice:8000/v1`. It is never published to
  the host, holds no data, and is versioned in lockstep with the stack. It is
  on by default (`COMPOSE_PROFILES=voice`).

The engine is not optional: it is the only way in. The core image,
`ghcr.io/thedancingdeveloper-org/vogt:0.6.1`, is also published at every
release because the stack image is built from it by digest and the release
manifest records both — it is a build input, not a deployment target.
`deploy/vogt.compose.yml` and `deploy/engine.overlay.yml` run a core and an
engine from a checkout for contributors; they are not a deployment.

**What the image is.** The stack image is a development pod, deliberately: an
agent session needs a machine, so it carries a writable home, a fixed
`sprooty` user (uid 1000), passwordless `sudo`, an SSH server, a Docker CLI
for a socket you may choose to mount, and the agent CLIs. It cannot run
`read_only` and does not drop capabilities. Treat it as you would a dev box:
keep the port on loopback or a private network, put something that terminates
TLS in front of it (§4), and give it only the mounts and credentials its
sessions need. [`SECURITY.md`](../SECURITY.md) describes the security model
this implies.

## 2. Quick start

```console
git clone https://github.com/TheDancingDeveloper-org/vogt
cd vogt
cp deploy/stack.env.example deploy/.env
$EDITOR deploy/.env                    # at minimum: ENGINE_TOKEN
openssl rand -hex 32 > deploy/vogt-core-token
docker compose -f deploy/stack.compose.yml up -d --wait
curl -fsS http://127.0.0.1:8910/readyz
```

There is no `--build`: the image already carries everything. Two secrets are
involved, and they are different shapes on purpose:

- **`ENGINE_TOKEN`** (in `deploy/.env`) is the engine's bearer token, at
  least 16 characters — what the browser and agents present. The stack refuses
  to start without it rather than inventing one.
- **`deploy/vogt-core-token`** is the core token, a *file* mounted as a
  Compose secret that both halves read: the engine presents it on
  `/api/vogt`, and the core adopts it at `init` as the actor and scopes named
  in `.env`. That is the whole first-boot bootstrap. Left empty, `/api/vogt`
  answers 401 until you mint a token by hand.

`up -d --wait` blocks until the health checks pass: the engine's `/readyz`,
and with voice on, the sidecar's `/health` (which reports `starting` until
its models load). `/readyz` reports the core's state but deliberately stays
ready when the core is absent — restarting the container would not revive a
core and would kill every live terminal — so read its body, not just the
status. Then open `http://localhost:8910/`, open **Settings**, paste
`ENGINE_TOKEN`, and save. [`USER_GUIDE.md`](USER_GUIDE.md) is the tour from
there.

Three named volumes — `vogt-data`, `engine-home` and `engine-agent-clis`
(§5) — survive `down`; do not add `--volumes` unless you mean to erase the
instance.

## 3. Configuration

Everything an operator chooses lives in `deploy/.env`, read by
`deploy/stack.compose.yml`. The keys that matter:

| Variable | Required | Default | Meaning |
|---|---|---|---|
| `ENGINE_TOKEN` | yes | — | The engine's bearer token, ≥16 characters. |
| `ENGINE_BIND` | no | `127.0.0.1` | Host interface the port is published on. Loopback until you mean to expose it. |
| `ENGINE_PORT` | no | `8910` | Host port the container's 8910 is published on. |
| `ENGINE_PUBLIC_URL` | no | — | The URL clients reach the stack at. Set it once there is a stable one (§4). |
| `VOGT_STACK_IMAGE` | no | `ghcr.io/thedancingdeveloper-org/vogt-stack:0.6.1` | The image to run. Pin a digest (§6). |
| `VOGT_VOICE_IMAGE` | no | `ghcr.io/thedancingdeveloper-org/vogt-voice:0.6.1` | The sidecar. Pin the same release as the stack. |
| `COMPOSE_PROFILES` | no | `voice` | Clear it to run without the sidecar; the voice controls stay present but inert. |
| `VOGT_BOOTSTRAP_CORE_TOKEN_ACTOR` | no | `agent:engine` | Who the adopted core token acts as. |
| `VOGT_BOOTSTRAP_CORE_TOKEN_SCOPES` | no | `read,work.write,project.write` | What it may do. Everything in the pod can read the file, so this is the blast radius. |
| `VOGT_HOOKS_REQUIRED` | no | `false` | Whether a missing lifecycle hook bundle is fatal. |
| `ENGINE_FCM_SERVICE_ACCOUNT_FILE` | no | — | Set to `/run/secrets/fcm_service_account` after placing `deploy/fcm-service-account.json` to enable native push. Web push needs nothing. |
| `ENGINE_ASSISTANT_STT_*`, `ENGINE_ASSISTANT_TTS_*` | no | the sidecar | Repoint speech at any OpenAI-compatible audio endpoint; a cloud provider wants `ENGINE_ASSISTANT_TTS_FORMAT=mp3`. |
| `VOGT_CLAUDE_CODE_VERSION`, `VOGT_CODEX_VERSION` | no | the baked version | An exact version is installed at start into `engine-agent-clis` and preferred over the image's copy. `latest`/`stable` are refused unless `VOGT_AGENT_CLI_ALLOW_DIST_TAGS=1`. |

Two prefixes, two processes. **`VOGT_*`** is read by the core; every setting
is in [`CONFIG.md`](CONFIG.md), and precedence is command line, then
environment, then the TOML named by `VOGT_CONFIG_FILE`, then defaults.
**`ENGINE_*`** is read by the engine; the authoritative list is
`engine/server/src/config.rs`, and [`ENGINE.md`](ENGINE.md) §3 covers the
TOML form. Anything the stack file does not pass through from `.env` — the
assistant's chat provider (`ENGINE_ASSISTANT_BASE_URL`, `_API_KEY`, `_MODEL`),
`ENGINE_ALLOWED_ORIGINS`, `ENGINE_VAPID_SUBJECT`, a forge token file, and so
on — goes in the `environment:` block of an overlay of yours (§7).

**Voice on or off.** `COMPOSE_PROFILES=voice` in `.env` starts the sidecar;
the stack file points the engine's STT and TTS URLs at it and asks for `wav`,
the only format its Piper backend serves. Clear the profile and the engine
finds nothing at those URLs and reports voice unavailable. Point the
`ENGINE_ASSISTANT_*_BASE_URLS` at another provider and you can clear the
profile too — the engine is bound to neither. The chat model behind the
assistant tab is separate and off until `ENGINE_ASSISTANT_API_KEY` and
`ENGINE_ASSISTANT_BASE_URL` are both set; a key with no base URL is a startup
error, not a silent default provider.

**Tokens for clients.** The core authenticates every request. A token is
bound to an actor, carries scopes (`read`, `work.write`, `project.write`,
`admin`, `writeback`), and is minted from the container that owns the data:

```console
docker compose -f deploy/stack.compose.yml exec vogt \
  vogt token issue --actor local:sprooty --name claude-code \
  --scopes read,work.write --reason "first agent credential"
```

The secret is shown once. Hand clients a file path rather than the value —
`vogt-mcp-remote` reads `VOGT_URL` and `VOGT_TOKEN_FILE`, and every token the
core itself holds is a `*_file` setting for the same reason: a token in the
environment is a token in every `docker inspect`. `vogt connect --format
markdown` renders the connection document for a running instance so nothing
about the address is hand-copied.

## 4. Reverse proxy and TLS

Exposure values carry no default. `ENGINE_BIND` stays on loopback until you
set it, and the engine cannot advertise an address it has not been told, so
set `ENGINE_PUBLIC_URL` to the URL clients actually use — `connect` and
`/connection-info` render against it. In the stack the core runs with
`VOGT_FRONTED=true` and takes its public identity from the engine per request,
so `VOGT_PUBLIC_URL` is deliberately absent from the stack file; it is the
equivalent setting for a core run on its own, not something to add here.

Put a TLS terminator in front of the published port and keep that port on
loopback so the proxy is the only way in. Whatever you use:

- forward WebSocket upgrades — terminal I/O is a WebSocket at
  `/api/sessions/{id}/attach`;
- do not buffer `/mcp`, a streaming transport (and force HTTP/1.1 or HTTP/2
  for it if an HTTP/3 edge misbehaves);
- leave `/healthz`, `/readyz`, and the core's `/health/ready` and `/version`
  as plain, unauthenticated HTTP for whatever probes you run;
- if the PWA is served from a different origin than the API, list that origin
  in `ENGINE_ALLOWED_ORIGINS`; same-origin use needs nothing.

A minimal Caddy site, with the stack published on the host's loopback:

```caddyfile
vogt.example.com {
    reverse_proxy 127.0.0.1:8910 {
        flush_interval -1
    }
}
```

Caddy forwards WebSocket upgrades without further configuration;
`flush_interval -1` disables response buffering so `/mcp` streams. With that
in place, `ENGINE_PUBLIC_URL=https://vogt.example.com` in `.env`.

## 5. Data, backup, upgrade

**What is on disk.** `vogt-data` is the core's `VOGT_DATA_DIR`
(`/var/lib/vogt`): `declared.sqlite3` — projects, work, tokens, the audit
log, the thing you cannot regenerate; `observed.sqlite3` — what collectors
recorded, regenerable by sweeping; `backups/`, where `vogt backup` writes;
and `repos/`, imported repositories. Both stores are SQLite in WAL mode, and
every write costs a checkpoint and its fsync; do not "fix" a slow sweep with
`VOGT_SQLITE_SYNCHRONOUS=off` in production — that trades durability for
speed in a product whose declared store is an audit log. `engine-home` holds
the engine's own state (session history, push subscriptions, agent tasks)
under `/home/sprooty/.local/share/vogt-engine`, alongside agent state and the
`Working` tree.

**Backup.**

```console
docker compose -f deploy/stack.compose.yml exec vogt \
  vogt backup --reason "nightly"
# → /var/lib/vogt/backups/<timestamp>/ with both stores and manifest.json
```

`backup` uses SQLite's online backup API, so it is consistent while the
server is running, and the manifest records each store's schema version. To
have it copy the engine's state too, set `VOGT_ENGINE_STATE_DIR` to the
engine's state directory in an overlay — both processes read that one
variable, and `/readyz`'s `backup_agreement` check confirms they agree.
Unset, the manifest says `not configured`, so a core-only backup never
pretends to be more. Copy the backup directory off the host: a backup on the
volume it protects is not a backup.

**Restore.**

```console
docker compose -f deploy/stack.compose.yml exec vogt \
  vogt restore --source /var/lib/vogt/backups/<timestamp> \
  --confirm --reason "restore after volume loss"
```

`restore` verifies the manifest before touching anything and refuses a
backup whose schema is *ahead* of the running build. Stop the traffic first
if you can, and restore from the container that owns the data directory.

**Migrations run at boot.** The entrypoint runs `vogt init` before every
`vogt serve`. `init` is idempotent: it creates the instance on a new volume,
brings an existing one forward to this build's schema, and leaves the audit
history alone after that. Until the schemas match, `/health/ready` answers
503 naming the store and both schema numbers. Migrations are forward-only and
run under a lock. Read [`SCHEMA.md`](SCHEMA.md) before a major version.

**Upgrade.**

1. Take a backup.
2. In `deploy/.env`, change `VOGT_STACK_IMAGE` **and** `VOGT_VOICE_IMAGE` to
   the new release's digests (§6). The pair is versioned together; a stack
   from one release with a sidecar from another is not a tested pair.
3. `docker compose -f deploy/stack.compose.yml pull`, then
   `docker compose -f deploy/stack.compose.yml up -d --wait`.
4. Watch `/readyz`. The sidecar carries no store and runs no migration, so its
   half of the step is only an image swap.

**Rollback** is the two digest lines reverted plus `up -d` — *unless* the
upgrade applied a migration. An older build against a newer store keeps
answering `ready`, but `vogt migrate` refuses the store, naming the
migration, and operations touching the changed tables fail. Rolling back
across a schema change means restoring the backup from step 1. Revert the
sidecar together with the stack so the two stay a pair.

### Upgrades and rollback: the forward-only limit

Migrations are **forward-only**. There is no down-migration, by design: the
migrator records `sha256` of every applied migration and verifies it on every
boot, so a schema only ever moves ahead. This bounds what a rollback can do.

- **A data volume the newer build has migrated cannot be served by the older
  image.** Once the upgrade applies migration `N`, the store carries an id the
  old build has never heard of. The old build's migrator refuses it —
  *"migration `N` is applied in the database but absent from this build … the
  database is ahead of the code"* — and stops there rather than running against
  a schema it does not understand.
- **So a same-image rollback only works when no migration ran.** If the upgrade
  applied nothing (the schema numbers already matched), reverting the two
  digest lines (§6) and `up -d` is the whole rollback. Check the upgrade's
  `/readyz` and `init` log: equal applied/bundled schema numbers, no new
  migration.
- **Across a schema change, roll back by restoring the backup from step 1**,
  then pin the *prior release's* stack **and** voice digests. Data written after
  the backup and before the rollback is lost — that is the cost of crossing a
  forward-only migration in reverse, and the reason step 1 is a backup, not a
  suggestion.

That an *existing* volume survives the forward direction cleanly — pending
migrations apply in order, already-applied ones stay byte-frozen, and the
recorded checksums still verify on the next boot — is covered by
`tests/test_upgrade_data_volume.py`, which upgrades a database built at the
oldest shipped migration set through the current migrator with its data intact.

**A second instance on the same host** is supported: give it its own Compose
project name, `--env-file`, port, public URL and core token file. The named
volumes are project-scoped, so a distinct `-p` already separates the data;
[`CUSTOMISATION.md`](CUSTOMISATION.md#running-a-second-instance-on-the-same-host)
has the details.

## 6. Digest pinning

The stack file names a tag so the example is runnable as-is. A deployment
should name a digest, because a digest is the only form of "which image is
this" that a rebuild cannot silently change — publishing an image and moving
a deployment are separate acts, and the digest line is what moves one.

Resolve the digests of a release:

```console
docker buildx imagetools inspect ghcr.io/thedancingdeveloper-org/vogt-stack:0.6.1 \
  | grep -m 1 Digest
docker buildx imagetools inspect ghcr.io/thedancingdeveloper-org/vogt-voice:0.6.1 \
  | grep -m 1 Digest
```

Verify the keyless signatures before starting Compose. Every release image is
signed by the release workflow's own OIDC identity, so the check constrains
both the workflow and the issuer, and performs an anonymous registry read:

```console
cosign verify \
  --certificate-identity-regexp '^https://github.com/TheDancingDeveloper-org/vogt/.github/workflows/release.yml@refs/tags/v[0-9].*$' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  ghcr.io/thedancingdeveloper-org/vogt-stack@sha256:<digest>
```

The same command applies to `vogt-voice@sha256:<digest>` unchanged. Then, in
`deploy/.env`:

```dotenv
VOGT_STACK_IMAGE=ghcr.io/thedancingdeveloper-org/vogt-stack@sha256:<digest>
VOGT_VOICE_IMAGE=ghcr.io/thedancingdeveloper-org/vogt-voice@sha256:<digest>
```

Pin both to the same release. An upgrade is then a change to those two lines
(§5); a rollback is the reverse, with the migration caveat.

## 7. Custom images and overlays

Customising the stack means layering on top, never editing
`stack.compose.yml`:

- **settings** go in `.env`, or in the `environment:` block of an overlay;
- **extra services, mounts and hooks** go in a Compose overlay of yours,
  layered with a second `-f`. Lifecycle hooks are mounted read-only at
  `/run/vogt/hooks` and never baked into the image
  ([`CUSTOMISATION.md`](CUSTOMISATION.md#deployment-owned-lifecycle-hooks));
- **extra tools** go in an image of yours that starts `FROM` the published
  stack digest, installs as `root`, and hands back to `USER sprooty`. Put
  tools in `/usr/local`, not under `$HOME` — the `engine-home` volume mounts
  over `/home/sprooty` and hides whatever the image wrote there.

A bind-mounted host directory arrives with the host's ownership, so anything a
session must write has to be writable by uid 1000; the uid is fixed in the
image, not a deploy-time choice.

[`CUSTOMISATION.md`](CUSTOMISATION.md#extending-the-stack-image) is the
guide, and
[`deploy/examples/custom-stack/`](../deploy/examples/custom-stack/README.md)
is a buildable worked example — a Dockerfile, an overlay, and a hook — that
CI builds and boots.

## 8. The demo image

`ghcr.io/thedancingdeveloper-org/vogt-demo` is a separate, static-only
artefact: the PWA compiled once, seeded with demo data, served by a small
read-only Node static server. It has no Python core, no Rust engine, no PTY,
no proxy and no write route — `/api/*` and `/mcp` answer 404, anything but
`GET`/`HEAD` answers 405 — and it needs no token and no volume. `demo-build.json`
and `demo-manifest.json` record the exact source SHA and asset hashes it was
built from. It is what the public demo sites run:

- <https://vogt-demo.thedancingdeveloper.com/> — the demo at `/`;
- <https://vogt-mobile-demo.thedancingdeveloper.com/> — the mobile showcase,
  `/mobile-demo.html` served as the root document, framing the same PWA at
  phone width.

One digest serves both. [`deploy/demo.overlay.yml`](../deploy/demo.overlay.yml)
runs it hardened (`read_only`, no capabilities, loopback by default on port
8912) and requires a digest-pinned `VOGT_DEMO_IMAGE`;
[`deploy/mobile-demo.overlay.yml`](../deploy/mobile-demo.overlay.yml) layers
on top of it and changes only the root document, so a second Compose project
from the same digest gives the mobile-first hostname:

```console
cp deploy/demo.env.example deploy/demo.env      # set VOGT_DEMO_IMAGE to a digest
docker compose -p vogt-demo --env-file deploy/demo.env \
  -f deploy/demo.overlay.yml up -d --wait
docker compose -p vogt-mobile-demo --env-file deploy/demo.env \
  -f deploy/demo.overlay.yml -f deploy/mobile-demo.overlay.yml up -d --wait
```

`/index.html` remains the real PWA in both, so the showcase cannot recurse
into itself. To build the artefact locally:

```console
docker build -f engine/Dockerfile --target demo-runtime \
  --build-arg VOGT_SOURCE_REF=main \
  --build-arg VOGT_SOURCE_SHA="$(git rev-parse HEAD)" .
```

The demo image is published by builds on `main` (§9), scanned and signed;
verify it with the `build.yml` workflow identity rather than the release one.

## 9. Releases

Development happens on `main`; releases are `v*` tags on commits reachable
from it. The release workflow checks that the tag matches the package version
and that the PWA, mobile and workflow metadata agree, then publishes, for
each of the core image (`vogt`), the stack (`vogt-stack`) and the sidecar
(`vogt-voice`):

- tags `X.Y.Z`, `X.Y` and `latest` — `latest` moves on a release and on
  nothing else;
- a keyless **cosign signature** over the digest, bound to
  `release.yml@refs/tags/v*` (§6);
- an **SBOM** and **provenance** attestation attached to the digest.

Each image is run before it is pushed — the stack must start both halves and
both agent CLIs, the sidecar must synthesise and transcribe with its baked
models — and the three versions must agree before the release is accepted.
The workflow then creates a GitHub Release carrying
`vogt-release-manifest.json` (the source SHA and all three digests) and the
signed Android APK of the mobile shell; the shell is a remote WebView onto a
deployed stack, so a server or PWA release reaches installed phones without a
new APK. The shell's lifecycle/connectivity/voice validation — the emulator
instrumentation suite and the device / Play pre-launch checklists — is in
[`mobile-release-validation.md`](mobile-release-validation.md).

Publishing is not deploying: a release changes nothing you run until you pin
its digests (§6).

Pushes to `main` are **builds, not releases**. `build.yml` publishes the same
three images tagged `sha-<commit>` (signed, with SBOM and provenance) and the
demo image as `main` and `main-<commit>`. No semver tag is created and
`latest` does not move, so "which build is that?" stays answerable. Pin the
release family for a deployment; a `sha-` image is a way to run a specific
commit, not a version.
