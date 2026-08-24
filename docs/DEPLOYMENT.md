# Deploying Vogt

How to run Vogt somewhere that is not your laptop: from the published image
or from source, what must be configured before it will start, what every
external dependency is for and what happens without it, and how to keep the
data safe across upgrades.

This is the operator's document. [`GETTING_STARTED.md`](GETTING_STARTED.md)
covers a first local run; [`CONFIG.md`](CONFIG.md) is the generated reference
for every core setting; [`CUSTOMISATION.md`](CUSTOMISATION.md) is how to layer
your own deployment onto the base without forking it; [`ENGINE.md`](ENGINE.md)
is the optional session engine in full.

## 1. What gets deployed

Vogt is one required component and two optional ones.

**The core** (required) — the Python service built by the root `Dockerfile`
and published as `ghcr.io/thedancingdeveloper-org/vogt`. One process serves
everything on one port:

```
vogt serve
  ├── /api/...     REST (FastAPI; OpenAPI at /openapi.json, UI at /docs)
  ├── /mcp         MCP streamable HTTP transport
  ├── /health/live, /health/ready, /version
  └── collector scheduler (in-process background sweeps)
```

The core serves the API and nothing else — no browser front end of its own.
The Solid PWA is served by the engine (below), which fronts the core.

Any port that serves MCP also serves plain HTTP health and version, so
`curl`, Compose health checks and uptime monitors work without an MCP client.
A core with nothing else configured is a complete, supported product.

**The session engine and PWA** (optional) — a Rust server under `engine/`
that embeds the web UI from `web/`, owns the terminals a work item's session
runs in, hosts the voice assistant, and can act as the front door in front
of the core. It is built from `engine/Dockerfile`; there is no published
image for it, but a generic Compose overlay builds and runs one beside the
core (§3.2). Without it the core's `session.*` operations report that no
engine is configured, and nothing else changes.

**The mobile shell** (optional) — a Capacitor wrapper under `mobile/` around
the same PWA. Nothing server-side depends on it. It loads the deployed front
door, so ordinary server and PWA releases reach installed phones without an
APK rebuild; the production APK procedure is in §7.1.

## 2. Run from published images

The supported self-hosting path is the Compose base at
[`deploy/vogt.compose.yml`](../deploy/vogt.compose.yml).

```console
git clone https://github.com/TheDancingDeveloper-org/vogt
cd vogt
cp deploy/.env.example deploy/.env
$EDITOR deploy/.env                   # at minimum: VOGT_PUBLIC_URL
docker compose -f deploy/vogt.compose.yml up -d
curl -fsS http://127.0.0.1:8080/health/ready
```

What the base does, and why it does it that way:

- **`vogt init && exec vogt serve`** is the container command. `init` is
  required, not decoration: `serve` on an empty data directory answers
  `/health/ready` with 503 and "run `vogt init` first". It is idempotent — it
  creates the instance on a new volume, brings an existing one forward, and
  leaves the audit history alone on every restart after that. `serve` also
  migrates both stores before it accepts traffic, so an image carrying a new
  migration cannot come up ready against an old schema.
- **Host exposure defaults to loopback.** The port is published on
  `${VOGT_BIND_IP:-127.0.0.1}:${VOGT_PORT:-8080}`. Set `VOGT_BIND_IP` to a
  real interface only when you mean to expose the instance, and read §6
  first.
- **`VOGT_PUBLIC_URL` is required** and the base refuses to start without it.
  The process cannot know the address clients reach it at, so it is asked
  rather than guessed.
- **The container is hardened by construction**: `read_only: true`,
  `cap_drop: [ALL]`, `no-new-privileges`, a 64 MB `noexec` tmpfs at `/tmp`,
  and one named volume at `/var/lib/vogt`. Those are the only writable paths.
- **The health check is plain HTTP** against `/health/ready` — no MCP
  handshake, no bearer token. A health check that needs a credential fails
  for the wrong reason.

### 2.1 The `.env` file

| Variable | Required | Default | Meaning |
|---|---|---|---|
| `VOGT_PUBLIC_URL` | yes | — | The URL clients use to reach this instance. |
| `VOGT_PORT` | no | `8080` | Host port the container's 8000 is published on. |
| `VOGT_BIND_IP` | no | `127.0.0.1` | Host interface the port is published on. |
| `VOGT_IMAGE` | no | `ghcr.io/thedancingdeveloper-org/vogt:v0.2.0` | The image to run. |
| `VOGT_UID` | no | `1000` | The uid the container runs as (gid is always 0). |
| `VOGT_LOG_LEVEL` | no | `info` | Verbosity of Vogt's own logger. |

Every other core setting (`VOGT_*`) can be added to the `environment:` block
of an overlay; [`CONFIG.md`](CONFIG.md) lists them all.

### 2.2 Pin a digest

The base names a tag so the example reads. A deployment should name a
digest, because a digest is the only form of "which image is this" that a
rebuild cannot silently change — publishing an image and moving a
deployment are separate acts, and the digest line is what moves one.

```console
docker buildx imagetools inspect ghcr.io/thedancingdeveloper-org/vogt:v0.2.0 \
  | grep -m1 Digest
# then, in deploy/.env:
VOGT_IMAGE=ghcr.io/thedancingdeveloper-org/vogt@sha256:<digest>
```

An upgrade is then a change to that one line plus `docker compose up -d`
(§7.3); a rollback is the reverse, with one caveat about schema migrations
that §7.4 spells out.

### 2.3 The uid

The image runs as **any uid as long as the gid is 0**. `/var/lib/vogt` is
owned by `root:0` and mode `0770`, so a fresh named volume — which Docker
seeds from the image directory's ownership — is writable whatever uid you
choose, and a restore onto a recreated volume does not break. Set `VOGT_UID`
to whoever owns the files this instance will observe if you bind-mount an
estate ([`CUSTOMISATION.md`](CUSTOMISATION.md#observing-an-estate-on-a-host-path)).

### 2.4 A second instance on the same host

Supported, and exactly the case where every host-wide default collides.
Give the second instance its own Compose project name, port and public URL:

```console
docker compose -p vogt-staging \
  --env-file deploy/staging.env \
  -f deploy/vogt.compose.yml up -d
```

The named volume and network in the base are project-scoped, so a distinct
`-p` already separates the data. What you must still change per instance is
`VOGT_PORT`, `VOGT_PUBLIC_URL`, and any host path or explicitly `name:`d
volume your own overlay adds — two writers on one SQLite database does not
stop the deploy and shows up later looking like corruption.

## 3. Build from source

### 3.1 The core

**With Compose.** Add the build overlay; it swaps the published image for a
build of the checkout and changes nothing else:

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

There is no published engine image, so the engine is always built from the
checkout. The generic overlay `deploy/engine.overlay.yml` does that and wires
the engine in front of the core in one command:

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

Be aware before you run it: the engine image is a **development pod**, not a
hardened service image — it carries a writable home, `sudo`, optional agent
CLIs, and an entrypoint that supports integrations this repository's
maintainer uses. It cannot be run `read_only` the way the core can.

## 4. Configuration

All core settings live in one schema, `src/vogt/config.py`, from which
[`CONFIG.md`](CONFIG.md) and `config.example.toml` are generated. Precedence,
highest first: command-line arguments, `VOGT_*` environment variables, the
TOML file named by `VOGT_CONFIG_FILE`, schema defaults.

The values you must decide before a network-facing deployment:

| Setting | Why you must set it |
|---|---|
| `VOGT_PUBLIC_URL` | An exposure value with no default. `connect` and `/connection-info` render client configuration against it. |
| `--host` / `--port` on `serve`, and `VOGT_BIND_IP` / `VOGT_PORT` in Compose | Also exposure; nothing in the image will bind an address for you. |
| `VOGT_UID` | Who owns the observed files (§2.3). |
| `VOGT_DATA_DIR` | Allocation, so it defaults (`/var/lib/vogt` in the image). One instance per directory. |

### 4.1 Tokens

The server authenticates every request by default; `--no-auth` exists for a
loopback listener and nothing else. A token is bound to an actor, carries
scopes (`read`, `work.write`, `project.write`, `admin`, `writeback`), and is
minted from the container that owns the data directory:

```console
docker compose -f deploy/vogt.compose.yml exec vogt \
  vogt token issue --actor local:alice --name claude-code \
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
| **GitHub** | Collects issues, pull requests, Actions and notifications for registered projects; enables write-back and `forge_*` operations | yes | Forge subjects read as *not collected*, never as absent; `notifications` is empty; write-back and account linking are unavailable | `VOGT_GITHUB_TOKEN_FILE` (a path). Other hosts: `forge_token_files` in TOML. Per-actor linked tokens: `VOGT_FORGE_ACCOUNT_KEY_FILE` |
| **Session engine** | Opens and attaches terminals for work items; the source of `session.*` | yes | `session.*` operations report "no engine configured"; nothing else changes | `VOGT_ENGINE_URL`, `VOGT_ENGINE_TOKEN_FILE`, optionally `VOGT_ENGINE_STATE_DIR` (so `backup` covers engine state) and `VOGT_SESSION_SCRATCH_PROJECT` |
| **MCP** (in-process) | Agents talk to this instance at `/mcp` on the same port, or over stdio via `vogt-mcp` | yes | Agents cannot connect; REST, CLI and GUI are unaffected | Nothing to enable; a token per client (§4.1) |
| **Remote MCP bridge** | `vogt-mcp-remote`, for clients that can only spawn a local process | yes | Use the HTTP transport instead — it needs nothing installed | `VOGT_URL`, `VOGT_TOKEN_FILE` in the client's environment |
| **A front door** | Anything that publishes the core at another address and paths (§6) | yes | Clients reach the core directly | `VOGT_FRONTED=true`; door sends `X-Vogt-Public-Url`, `X-Vogt-Api-Path`, `X-Vogt-Mcp-Path` |
| **Bootstrap core token** | Lets a fronted deployment come up in one deploy instead of mint-then-redeploy | yes | The front door's `/api/vogt` answers 401 until a token is minted by hand | `VOGT_BOOTSTRAP_CORE_TOKEN_FILE`, `_ACTOR`, `_SCOPES` |

Beyond these the core needs only SQLite (bundled with Python) and `git`
(in the image). There is no external database, cache or queue.

### 5.2 Engine integrations

These are read by the engine process, not the core. `deploy/engine.overlay.yml`
(§3.2) already wires the common ones from `deploy/.env`, so for a Compose
deployment you set these in `.env` rather than by hand. `ENGINE_*` is the
current prefix; legacy `MYDEVENV2_*` names are still accepted as aliases for
one release and log a warning. Each can also be set in the engine's TOML config
file under the same name without the prefix ([`ENGINE.md`](ENGINE.md) §3).

| Integration | Purpose | Optional | When absent | Configured by |
|---|---|---|---|---|
| **Engine token and bind** | The engine's own bearer token (≥16 chars) and listen address | token required | The engine refuses to start | `--token`, `--bind` (default `127.0.0.1:8910`) and `--config` flags, or `ENGINE_TOKEN` / `ENGINE_BIND` / `ENGINE_CONFIG` in the environment (legacy `MYDEVENV2_TOKEN` / `MYDEVENV2_BIND` / `MYDEVENV2_CONFIG` still accepted with a deprecation warning), or the config file. Scoped extra tokens: `ENGINE_EXTRA_TOKENS_JSON`; write-rate cap: `ENGINE_MUTATING_REQUEST_LIMIT_PER_MINUTE` |
| **The core behind it** | Proxies `/api/vogt` and `/mcp` to a core, injecting the core token for `/api/vogt` | yes | The engine runs alone; `/readyz` reports the core's state and stays ready (an absent core must not cost running terminals); Vogt routes answer 503 naming the reason | `VOGT_CORE_URL` (loopback → the entrypoint also *runs* the core there; anything else → proxy only), `VOGT_CORE_TOKEN_FILE` (preferred) or `VOGT_CORE_TOKEN`, `ENGINE_PUBLIC_URL`, `VOGT_IMPORT_ROOT`, `VOGT_ENGINE_STATE_DIR` |
| **Voice assistant provider** | The chat model behind the assistant tab and spoken requests | yes | The assistant routes answer 404 and the PWA hides the tab | `ENGINE_ASSISTANT_API_KEY`, `ENGINE_ASSISTANT_BASE_URL`, `ENGINE_ASSISTANT_MODEL` — any **OpenAI-compatible chat endpoint**. A key with no base URL is a startup error, not a silent default provider. Tuning: `ENGINE_ASSISTANT_MAX_TOOL_CALLS`, `ENGINE_ASSISTANT_REASONING_EFFORT`, `ENGINE_ASSISTANT_LOG_RETENTION_DAYS`; several providers at once: `ENGINE_ASSISTANT_PROFILES_JSON`, `ENGINE_ASSISTANT_DEFAULT_PROFILE` |
| **Speech-to-text** | Server-side transcription for voice input | yes | Voice input is unavailable; text chat unaffected | `ENGINE_ASSISTANT_STT_BASE_URLS` (comma-separated, ordered fallback — empty means off), `ENGINE_ASSISTANT_STT_API_KEY`, `ENGINE_ASSISTANT_STT_MODEL` (default `whisper-1`); OpenAI-compatible audio endpoint, need not be the chat provider |
| **Text-to-speech** | Spoken replies | yes | Replies are text only | `ENGINE_ASSISTANT_TTS_BASE_URLS`, `ENGINE_ASSISTANT_TTS_API_KEY`, `ENGINE_ASSISTANT_TTS_MODEL` (default `tts-1-hd`), `ENGINE_ASSISTANT_TTS_VOICE` (default `nova`), `ENGINE_ASSISTANT_SPEECH_TIMEOUT_MS` |
| **Push notifications** | Web push to browsers; native push via Firebase Cloud Messaging | yes | Web push works with no configuration beyond a sensible `ENGINE_VAPID_SUBJECT`; without FCM only the native transport is disabled | `ENGINE_VAPID_SUBJECT` (a `mailto:`, default `mailto:admin@example.invalid`), `ENGINE_FCM_SERVICE_ACCOUNT_JSON` |
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

### 7.1 Promote `dev` to production

1. Promote the approved `dev` commit to `prod` and wait for its CI run to
   pass. Create the release tag only on that commit: the release workflow
   rejects a tag whose target is not reachable from `prod`.
2. Back up the running instance and copy the resulting backup directory off
   its data volume:

   ```console
   docker compose -f deploy/vogt.compose.yml exec vogt \
     vogt backup --reason "pre-production release"
   ```

3. Increase the package version if a new Android artifact is needed, then
   create and push the matching `v<version>` tag. The tagged release publishes
   signed, immutable core and merged-stack image digests. It also builds the
   signed APK when its release prerequisites are configured.
4. In the estate deployment repository, change the production stack's image
   pin to the exact merged-stack digest from the release summary, review that
   change under the estate's approval policy, and run its `DeployStack` for
   `personal-vogt`. That repository and Node B are operator infrastructure;
   public/self-hosted deployments remain independent of this handoff.
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
- Increase `mobile/package.json`'s SemVer version. Gradle derives the Android
  `versionCode` from it, so each published upgrade must be greater than the
  last.
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
docker compose -f deploy/vogt.compose.yml exec vogt \
  vogt backup --reason "nightly"
# → /var/lib/vogt/backups/<timestamp>/ with both stores and manifest.json
```

`backup` uses SQLite's online backup API, so it is consistent while the
server is running. The manifest records the schema version of each store;
if `VOGT_ENGINE_STATE_DIR` is set and readable, the engine's state is copied
too and the manifest says so — otherwise it says `not configured`, so a
core-only backup never pretends to be more. Copy the backup directory off
the host; a backup on the volume it protects is not a backup.

```console
docker compose -f deploy/vogt.compose.yml exec vogt \
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
2. Change the digest (or tag) in `deploy/.env`.
3. `docker compose -f deploy/vogt.compose.yml up -d`.
4. Watch `/health/ready`: it answers 503 — naming the store and both
   schema numbers — until `init` and the startup migration complete,
   then 200 with the applied and expected schema versions. Migrations
   are forward-only and run under a lock, so two containers starting at once
   cannot race.

Read [`SCHEMA.md`](SCHEMA.md) before a major version: it lists every
migration and what each changes.

### 7.5 Rollback

A rollback is the digest line reverted plus `up -d` — **unless the upgrade
applied a migration**. Migrations are forward-only: an older build against a
newer store keeps answering `ready` (deliberately, so a deliberate rollback
does not look like a broken container), but `vogt migrate` refuses the
store, naming the migration, and operations that touch the changed tables
fail. Rolling back across a schema change means restoring the backup from
step 1, not just running the older image.

## 8. Troubleshooting

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
