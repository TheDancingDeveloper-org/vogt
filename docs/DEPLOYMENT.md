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
    ├── /api/vogt/...   proxied to the core, the caller's bearer forwarded
    ├── /mcp            proxied to the core
    ├── /api/auth/login, /api/install/...   proxied untouched; the core self-gates
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
`ghcr.io/thedancingdeveloper-org/vogt:0.7.7`, is also published at every
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
$EDITOR deploy/.env                    # optional: port, bind, a break-glass token
openssl rand -hex 32 > deploy/vogt-core-token
docker compose -f deploy/stack.compose.yml up -d --wait
curl -fsS http://127.0.0.1:8910/readyz
```

There is no `--build`: the image already carries everything. One secret is
required and one is optional, and they are different shapes on purpose:

- **`deploy/vogt-core-token`** is the **stack secret**, a *file* mounted as
  a Compose secret that both halves read and nobody else ever holds. The
  core adopts it at `init` as the actor and scopes named in `.env` and calls
  the engine with it to start sessions; the engine recognises it as the
  core's own identity, follows the core's event feed with it, and lends it
  to the break-glass token below. That is the whole first-boot bootstrap.
  Put a new value in the file and the next boot adopts it and revokes the
  old one. Left empty, the two halves have no credential for each other —
  the core cannot start sessions and the engine cannot follow events — while
  people can still sign in.
- **`ENGINE_TOKEN`** (in `deploy/.env`) is optional: a static **break-glass**
  credential for the engine, at least 16 characters, with full capability
  and no actor of its own — its Vogt calls are attributed to the stack
  secret's actor. Nothing a person or an agent presents day to day is set
  here: people sign in with a password and agents hold API tokens, both
  checked by the core. Set it only for a way in that does not depend on
  the core; leave it empty and every credential is a core token.

`up -d --wait` blocks until the health checks pass: the engine's `/readyz`,
and with voice on, the sidecar's `/health` (which reports `starting` until
its models load). `/readyz` reports the core's state but deliberately stays
ready when the core is absent — restarting the container would not revive a
core and would kill every live terminal — so read its body, not just the
status. Then open `http://localhost:8910/`: the first-run wizard asks for
your name, a username and a password, creates your `admin` login and signs
you in. The wizard stays open until the first *person* has a login: the
stack secret is bound to an agent actor and does not count, so supplying it
before `up` no longer closes the wizard (releases up to v0.7.7 did, #903).
`GET /api/install/status` says which: `{"install_mode": true}` while the
wizard is offered. Once closed it stays closed — the store latches it, and
an upgrade from v0.7.7 or earlier latches any store that already held a
token, so a running instance operated only through `ENGINE_TOKEN` is never
reopened. If it answers `false` with nobody able to sign in — such an
upgraded instance, or `VOGT_INSTALL_BOOTSTRAP_ENABLED=false` — create the
first operator in the container instead; it prompts for the password:

```console
docker compose -f deploy/stack.compose.yml exec vogt \
  vogt user create --username <name> --scopes admin --reason "Create first operator"
```

The wizard is an unauthenticated door until it closes, so on a stack
published beyond loopback (§4) either finish it before opening the port up,
or set `VOGT_INSTALL_BOOTSTRAP_ENABLED=false` in an overlay and use the
command above. Later people are added with `vogt user create` and agents
get API tokens from `vogt token issue`, both run inside the container (§3).
[`USER_GUIDE.md`](USER_GUIDE.md) is the tour from there.

Three named volumes — `vogt-data`, `engine-home` and `engine-agent-clis`
(§5) — survive `down`; do not add `--volumes` unless you mean to erase the
instance.

## 3. Configuration

Everything an operator chooses lives in `deploy/.env`, read by
`deploy/stack.compose.yml`. The keys that matter:

| Variable | Required | Default | Meaning |
|---|---|---|---|
| `ENGINE_TOKEN` | no | — | An optional static break-glass token for the engine, ≥16 characters: full capability, no actor of its own. Unset, every credential is a core token checked by the core. |
| `ENGINE_BIND` | no | `127.0.0.1` | Host interface the port is published on. Loopback until you mean to expose it. |
| `ENGINE_PORT` | no | `8910` | Host port the container's 8910 is published on. |
| `ENGINE_PUBLIC_URL` | no | — | The URL clients reach the stack at. Set it once there is a stable one (§4). |
| `VOGT_STACK_IMAGE` | no | `ghcr.io/thedancingdeveloper-org/vogt-stack:0.7.7` | The image to run. Pin a digest (§6). |
| `VOGT_VOICE_IMAGE` | no | `ghcr.io/thedancingdeveloper-org/vogt-voice:0.7.7` | The sidecar. Pin the same release as the stack. |
| `COMPOSE_PROFILES` | no | `voice` | Clear it to run without the sidecar; the voice controls stay present but inert. |
| `VOGT_BOOTSTRAP_CORE_TOKEN_ACTOR` | no | `agent:engine` | Who the adopted stack secret acts as — and therefore whom a break-glass token's Vogt calls are attributed to. |
| `VOGT_BOOTSTRAP_CORE_TOKEN_SCOPES` | no | `read,work.write,project.write` | What it may do. Everything in the pod can read the file, so this is the blast radius. |
| `VOGT_HOOKS_REQUIRED` | no | `false` | Whether a missing lifecycle hook bundle is fatal. |
| `ENGINE_FCM_SERVICE_ACCOUNT_FILE` | no | — | Set to `/run/secrets/fcm_service_account` after placing `deploy/fcm-service-account.json` to enable native push. Web push needs nothing. |
| `ENGINE_AGENT_CLAUDE_SETTINGS` | no | the image's policy | The driven-session permission policy given to every engine-launched Claude session: a Claude Code settings file whose `autoMode` lists start with `"$defaults"`. Name your own to list the repositories agents may merge their own green PRs in, and your estate's secret stores, deploy targets and sensitive hosts. `off` turns it off ([`ENGINE.md`, "Permission posture"](ENGINE.md#permission-posture)). |
| `ENGINE_AGENT_GRANT_PROJECTS` | no | none | Secrets-manager projects, besides the ones `ENGINE_AGENT_AUTH_SECRETS` already names, that a person-approved grant may fetch a secret from (space- or comma-separated ids). Unset, grants reach only the manifest's projects ([`ENGINE.md` §9](ENGINE.md)). |
| `ENGINE_METRICS_ADDR` | no | off | Serve `GET /metrics` (Prometheus text) on this address, a listener of its own, never the API port: session launch latency and its stages ([`ENGINE.md`, "Launch timing, logs and metrics"](ENGINE.md#launch-timing-logs-and-metrics)). `0.0.0.0:9464` inside the container; publish or route the port only where your scraper sits. |
| `ENGINE_AGENT_OPENCODE_CONFIG` | no | the image's policy | The opencode half of the driven-session policy: an opencode config with a `permission` block, given to every engine-launched opencode session as `OPENCODE_CONFIG_CONTENT` (the user's own config is not edited). Allow `gh pr merge` here where agents may merge their own PRs. `off` turns it off ([`ENGINE.md`, "Permission posture"](ENGINE.md#permission-posture)). |
| `ENGINE_SESSION_RSS_WARN` | no | off | Flag any session whose process tree's resident memory reaches this (`8GiB`). Usage is shown either way; this only marks the heavy ones. |
| `ENGINE_HIBERNATE_IDLE_AFTER` | no | off | Hibernate an agent session quiet this long (`2h`, `30m`): its processes stop, it stays listed, and a wake resumes its conversation ([`ENGINE.md`, "Hibernation"](ENGINE.md#hibernation)). Sessions running a turn, at a permission dialog, blocked on a person, running a tool shell, pinned awake or on autopilot are exempt (autopilot only from this idle trigger). |
| `ENGINE_AUTOPILOT_NUDGE_AFTER`, `ENGINE_AUTOPILOT_MAX_NUDGES` | no | `60s`, `100` | How long an autopilot session sits at its prompt before the engine tells it to carry on, and the most times it does; `0` never nudges ([`ENGINE.md`, "Autopilot"](ENGINE.md#autopilot)). |
| `ENGINE_AGENT_CONVERSATION_HOOK` | no | `1` | Whether the entrypoint installs Claude Code's `SessionStart`/`SessionEnd` hook into the pod user's settings, so a `claude` typed into a plain shell reports its conversation and the session becomes resumable across a restart; `0` skips it ([`ENGINE.md`, "A conversation reported from inside the session"](ENGINE.md#a-conversation-reported-from-inside-the-session)). |
| `ENGINE_HIBERNATE_MEMAVAILABLE_BELOW` | no | off | While the pod's available memory is below this (`2GiB`), hibernate the quietest eligible session, one a minute, with the same exemptions. |
| `ENGINE_ASSISTANT_STT_*`, `ENGINE_ASSISTANT_TTS_*` | no | the sidecar | Repoint speech at any OpenAI-compatible audio endpoint; a cloud provider wants `ENGINE_ASSISTANT_TTS_FORMAT=mp3`. |
| `VOGT_CLAUDE_CODE_VERSION`, `VOGT_CODEX_VERSION`, `VOGT_OPENCODE_VERSION`, `VOGT_GO_VERSION`, `VOGT_KLAUDIA_VERSION` | no | the baked version | An exact version is installed at start into `engine-agent-clis` and preferred over the image's copy. `latest`/`stable` are refused unless `VOGT_AGENT_CLI_ALLOW_DIST_TAGS=1`. `VOGT_GO_VERSION` (`1.27.1`) is a Go release from go.dev, checked against the sha256 its release index gives; `latest` there is the newest stable Go. `VOGT_KLAUDIA_VERSION` is a full commit id, built from source at start (a minute or so the first time; the module cache stays on the volume). |

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

**Credentials for people and agents.** The core authenticates every request
and is the only identity authority; the engine holds no token table and asks
the core who a bearer is. **People sign in with a username and password.**
The first operator chooses theirs in the browser wizard (or, where the
wizard is off, with the `vogt user create --scopes admin` command in §2);
every later person is created from the container that owns the data:

```console
docker compose -f deploy/stack.compose.yml exec vogt \
  vogt user create --username ada --display-name "Ada Lovelace" \
  --scopes read,work.write,project.write --reason "Ada joins the team"
```

That prompts for the password on the terminal; `--password-file PATH` and
`--password-stdin` are the non-interactive forms, and a password is never
accepted as an argument. Signing in mints a **session** — a core token bound
to the person's own actor that expires after the core's `session_ttl_days`
(`VOGT_SESSION_TTL_DAYS`, 30 by default; [`CONFIG.md`](CONFIG.md)) and is
revoked by signing out. `vogt user passwd` replaces a password and ends the
person's sessions; `vogt user remove` takes the login away and keeps the
actor and its audit history; `vogt user list` never shows a hash.
**Agents, scripts and MCP clients hold API tokens.** A token is bound to an
actor, carries scopes (`read`, `work.write`, `project.write`, `admin`,
`writeback`), and is minted the same way:

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

**Read-only MCP servers for agents.** The image carries pinned GitHub,
Grafana and Gitea/Forgejo MCP servers, and each session registers the ones
whose token it holds, read-only ([`ENGINE.md`](ENGINE.md) §4). Nothing is on
by default. To turn one on, put its variables in the session's environment —
with the reference agent-auth helper, as launch-time lines in
`ENGINE_AGENT_AUTH_SECRETS` (not `ondemand`: the registration happens at
launch), with the non-secret URLs in the overlay's `environment:`:

```text
GITHUB_MCP_TOKEN               <project-id> <github-readonly-pat-name>
GRAFANA_SERVICE_ACCOUNT_TOKEN  <project-id> <grafana-viewer-token-name>
GITEA_MCP_TOKEN                <project-id> <forgejo-readonly-token-name>
```

```yaml
environment:
  GRAFANA_URL: https://grafana.example
  GITEA_HOST: https://forge.example
```

Issue every one of these tokens read-only — a fine-grained GitHub PAT with
read permissions on the repositories agents may see, a Grafana service
account with the Viewer role, a Forgejo token with `read:` scopes only. The
servers run in their read-only modes as well, but the token is the control
that holds if a mode has a gap. `git-forgejo` (git with a forge token header
that cannot be word-split) uses `FORGEJO_TOKEN` and `FORGEJO_URL` instead,
because it pushes as well as reads.

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

### Cloning prod into dev

`restore` is for putting an instance back. Pointed at *another* instance's
backup it makes the target become that instance: the source's API tokens and
password logins work on the target and the target's own are gone, projects
with forge write-back armed send the target's edits upstream as if from the
source, and the two answer with one instance id. `vogt clone` is the restore
for a copy:

```console
vogt clone --source /var/lib/vogt/backups/<label> \
  --confirm --reason "clone prod into dev" [--include-engine-state]
```

It verifies the manifest exactly as `restore` does (an older schema is
migrated forward, a newer one is refused, nothing is touched before the
checks pass) and refuses a backup of the target itself. It then copies the
stores into a staging directory beside the live ones, migrates them there,
and sanitises the copy in **one audited write** before swapping it in, so a
failure part-way leaves the live stores as they were:

- **Credentials stay the target's.** Every live token the source had is
  revoked in the copy (revoked rather than deleted, so `auth_decisions`
  still resolves); its password logins and linked forge accounts are
  dropped. The target's own tokens, password logins and forge accounts are
  carried in, with their actors matched by `identity_ref`. A secret both
  instances hold — one stack secret on both — stays live as the target's.
- **Forge write-back is disarmed**: every project's `write_back` becomes
  `none`. Re-arm a project deliberately with `vogt forge writeback`.
- **Running sessions are closed**: a session the source recorded as running
  is a process on the source's engine, so the copy marks it stopped.
- **The target keeps its instance id**, and `meta` records the clone stamp —
  source instance id, clone time and the backup's as-of time — which
  `vogt status` reports as `clone`. The audit log carries a `clone` row and
  the feed an `instance.cloned` event.
- **Engine state.** Without `--include-engine-state` the target engine's
  state is untouched. With it, only session history is copied
  (`history.db`, `session-logs/`, `assistant-log.db`); the source's
  `push.json` (its phones' push subscriptions and VAPID keys) and its agent
  tasks are never copied, so the copy notifies the target's devices and
  does not run the source's scheduled work.

`clone` is local-only like `restore`: the CLI in the container that owns the
data directory, never REST or MCP.

**The estate procedure.** Prod is the source, dev the target. Run it from a
shell *outside* the dev stack: step 6 recreates the dev container and every
session in it.

1. **Dev runs a build with `vogt clone`**, at a schema at or past prod's.
   Check with `vogt migrate` (or `/readyz`) on both.
2. **Freeze prod, then back it up.** Stop writing to prod first, so the
   backup is the last word. Then, on the prod host:

   ```console
   docker exec <prod-container> vogt backup --label prod-to-dev-<date> \
     --reason "clone prod into dev"
   ```

   It lands in `/var/lib/vogt/backups/prod-to-dev-<date>/`. The manifest's
   `engine_state` line says whether the engine's state came too
   (`VOGT_ENGINE_STATE_DIR`, above).
3. **Move the backup directory to dev's data volume.**

   ```console
   docker cp <prod-container>:/var/lib/vogt/backups/prod-to-dev-<date> .
   # copy ./prod-to-dev-<date> to the dev host (scp/rsync), then there:
   docker cp prod-to-dev-<date> <dev-container>:/var/lib/vogt/backups/
   ```

   The clone only reads the directory, so root ownership from `docker cp`
   is fine.
4. **Back up dev**, so the clone is reversible:
   `docker exec <dev-container> vogt backup --label pre-clone-<date> --reason "before clone"`.
5. **Clone.**

   ```console
   docker exec <dev-container> vogt clone \
     --source /var/lib/vogt/backups/prod-to-dev-<date> \
     --confirm --reason "clone prod into dev"
   ```

   Add `--include-engine-state` to bring prod's session history. Read the
   result: `source_tokens_revoked`, `write_back_reset`, `engine_state` and
   the two `import_root` lines. Differing import roots mean the project
   paths in the copy do not exist on dev.
6. **Restart dev** (redeploy the stack, or `docker restart <dev-container>`)
   so the engine and core reopen the new stores. The entrypoint's
   `vogt init` re-adopts dev's bootstrap tokens, as on any boot.
7. **Verify.** `docker exec <dev-container> vogt status` shows dev's own
   `instance_id` and a `clone` block naming prod. `/readyz` is ready. A dev
   token works and a prod token gets 401. `vogt project list` shows every
   `write_back` as `none`. The work items, comments and initiatives you
   expect are there.

**Which copy is live.** After the clone, dev is the working copy and prod is
frozen. Nothing keeps the two in step. The way back is a cut-over, a clone in
the other direction, or a merge with `vogt import` (below), which carries
dev's work into a prod that kept running.

**Agent context** (transcripts, per-project memory, Codex sessions, the notes
in `~/Working`) lives on the engine home volume, not in the stores.
`scripts/clone-agent-context.sh SRC_HOME DST_HOME` copies an allowlist of it
between two homes, local or one side over rsync/ssh. It never copies
credentials, MCP configuration, settings or caches. It is a dry run unless
given `--apply`, and it warns when project paths do not match. Run it while no
session is writing on either side, and after the clone. The script's header
lists exactly what it copies.

### Export and import: merging one instance into another

`vogt export` writes the declared entities as JSON; `vogt import` merges such
a file into a live instance. Unlike `restore` and `clone`, nothing is
replaced: both sides' work survives, under the policy below.

```console
vogt export --destination /var/lib/vogt/exports/dev.json \
  [--project <slug>] --reason "carry dev back to prod"
vogt import --source /var/lib/vogt/exports/dev.json [--project <slug>] \
  --reason "carry dev back to prod"                  # dry run: the report
vogt import --source ... --apply --confirm [--strict] \
  --reason "carry dev back to prod"                  # writes it
```

**The export (format 2)** carries projects, work items — with their labels,
relations and initiative link — initiatives, labels, actors, every comment,
each entity's `created_at`/`updated_at`, and the instance's clone stamp. It
never carries tokens, password logins, forge accounts, auth decisions or
sessions. `--project` exports one project's items with their comments, and
only the initiatives, labels and actors they reference. An export written
before format 2 (no `export_format_version` key) is still readable: `import`
reports what it holds and refuses to apply it.

**Matching.** Entities match on identity that is stable across instances:
projects by slug, work items by id (never by `WI-n`, which each instance
numbers on its own), initiatives by slug, labels by name, actors by
`identity_ref`, comments by id, relations by (from, kind, to).

**The conflict policy.** "Changed" means an entity's `updated_at` is later
than the *baseline*, the moment the two instances last agreed. The baseline
is the clone stamp's backup time when one instance is a clone of the other
(in either direction), or the export's own `exported_at` for an export of the
same instance. Unrelated instances have no baseline. The import reports which
it used (`base`, `base_source`).

| Case | Result |
|---|---|
| only in the export | **created**; a work item gets a fresh ref here, and the report names both refs |
| identical on both sides | unchanged (counted, not listed) |
| changed only in the export | **updated** to the incoming version |
| changed only here | **skipped**: this instance's version kept |
| changed on both sides, or no baseline | **conflict**: this instance's version kept; for a work item the incoming version is attached as a comment, once |
| any conflict, with `--strict` | the whole import refused, nothing written |
| comment, relation, label, actor not here | **created** (append-only) |

The merge is **additive**: it never deletes a work item, comment, relation
or label, and a project's `root_path` is never changed (a difference is
reported). **Never imported:** tokens, password logins, forge accounts,
`write_back` (a created project starts at `none` and unlinked), push
subscriptions, sessions, the instance id and the clone stamp. Items retired
upstream (`superseded_by`) and items of a project that is upstream-truth here
are skipped; the forge holds those. A work item whose state the target's
workflow does not know is skipped and named.

**Safety.** Without `--apply` the import is a dry run and writes nothing.
`--apply` requires `--confirm` and a reason, and lands as **one audited
write** (an `import` audit row and an `instance.imported` event): a failure,
including a `--strict` refusal, leaves the target untouched. A re-import of
the same file changes nothing — a standing conflict is reported again, but
its comment is not repeated. Like `restore` and `clone` it is local-only:
the CLI where the data directory is, never REST or MCP. Nothing runs an
import automatically.

**Carrying dev back to prod.** Prod is the target, dev the source; dev must be
a clone of prod (`vogt status` on dev shows a `clone` block naming prod's
instance id), otherwise every difference is a conflict.

1. **Prod and dev run the same build**, or prod a newer one.
2. **Back up prod** so the merge is reversible:
   `docker exec <prod-container> vogt backup --label pre-import-<date> --reason "before import"`.
3. **Export dev** (whole, or `--project <slug>`):
   `docker exec <dev-container> vogt export --destination /var/lib/vogt/exports/dev-<date>.json --reason "carry dev back to prod"`,
   then copy the file into prod's container (`docker cp`, as in the clone
   procedure).
4. **Dry run on prod**, and read the report:
   `docker exec <prod-container> vogt import --source /var/lib/vogt/exports/dev-<date>.json --reason "carry dev back to prod"`.
   Check `base_source` names the clone, and read every `conflict` and
   `skipped` entry.
5. **Apply**: the same command with `--apply --confirm` (add `--strict` to
   refuse rather than record conflicts). No restart is needed; the write is
   an ordinary audited transaction.
6. **Resolve conflicts** by reading each conflicted item's `Import conflict`
   comment and editing the item. After carrying dev back, re-clone dev from a
   fresh prod backup: the clone baseline is now behind both sides, so a
   second carry would report every item touched by the first as a conflict.

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
project name, `--env-file`, port, public URL and stack secret file. The named
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
docker buildx imagetools inspect ghcr.io/thedancingdeveloper-org/vogt-stack:0.7.7 \
  | grep -m 1 Digest
docker buildx imagetools inspect ghcr.io/thedancingdeveloper-org/vogt-voice:0.7.7 \
  | grep -m 1 Digest
```

Verify the keyless signatures before starting Compose. Every release image is
signed by the release workflow's own OIDC identity, so the check constrains
both the workflow and the issuer, and performs an anonymous registry read.
(The same digest also carries the signature `build.yml` made when it built it
on `main` — a release promotes that build rather than rebuilding, §9 — but
the release identity is the one that says "this is version X".)

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
from it. A release publishes, for each of the core image (`vogt`), the stack
(`vogt-stack`) and the sidecar (`vogt-voice`):

- tags `X.Y.Z`, `X.Y` and `latest` — `latest` moves on a release and on
  nothing else (a pre-release tag such as `v1.0.0-rc.1` gets only its exact
  version);
- a keyless **cosign signature** over the digest, bound to
  `release.yml@refs/tags/v*` (§6);
- an **SBOM** and **provenance** attestation attached to the digest.

### Release process: build once, promote by digest

A release does not build images. The bytes a release ships are the bytes that
were built, smoke-tested and deployed to a development lane before the tag
existed:

1. **Bump on `main`.** The version bump lands on `main` like any change, so
   the `build.yml` run for that commit bakes the release version into the
   images.
2. **Build once.** `build.yml` builds the three images for that commit, runs
   each before pushing it — the stack must start both halves and both agent
   CLIs, the sidecar must synthesise and transcribe with its baked models —
   then publishes them as `sha-<7-char commit>` with SBOM and provenance and
   signs each digest under `build.yml@refs/heads/main`.
3. **Validate.** A development deployment pins those digests and runs its
   smoke test against them.
4. **Tag** `vX.Y.Z` on that same commit. `release.yml` then:
   - checks the tag is reachable from `main` and matches the package, PWA,
     mobile and workflow versions (`scripts/check_product_version.py`, which
     also proves `build.yml` baked that version);
   - finds a `build.yml` run for the exact commit on `main` whose three
     image jobs succeeded (the demo image's job is not required), resolves
     each `sha-<commit>` tag to its digest, and verifies that digest carries
     `build.yml`'s signature **for that commit** (the certificate's
     workflow-sha) plus its SBOM and provenance attestations;
   - retags each digest `X.Y.Z`, `X.Y` and `latest` with
     `docker buildx imagetools create --prefer-index=false <repo>@<digest>` —
     a registry-side copy of the original manifest, no pull and no rebuild —
     and asserts every new tag resolves to exactly the source digest;
   - signs those same digests with cosign under its own identity, so both a
     build signature and a release signature sit on one digest;
   - builds and signs the Android APK (the app is not a container image, so
     it is still built at tag time; `release-mobile.yml` builds the Play AAB
     the same way) and writes a GitHub Release carrying the APK and
     `vogt-release-manifest.json`.

`vogt-release-manifest.json` keeps every key of schema
`vogt-release-manifest.v1` (`source_sha`, `images.{core,merged_stack,voice}`
as `<repo>@<digest>`) and adds `image_digests`, `release_tags` and a
`promotion` block naming the `build.yml` run and `sha-` tag the digests came
from (`rebuilt: false`). A deployment can match `source_sha` and the three
digests against the receipt of the development deploy that ran them, and
refuse anything else.

**Failure modes, all fail-closed — a release never falls back to a rebuild:**

| Symptom | Cause | Recovery |
| ------- | ----- | -------- |
| `build.yml never ran for <sha> on main` | the tagged commit only touched docs (`build.yml` ignores `docs/**` and `*.md`), or a newer push cancelled its build before it started | tag the commit `build.yml` built (normally the bump), or dispatch `build.yml` on `main` while that commit is its head; then re-run the release |
| `did not publish every image` | an image job of that `build.yml` run failed or was cancelled | `gh run rerun <id> --failed`, then re-run the failed release jobs |
| `<repo>:sha-<short> is not in the registry` | the tag was never pushed or has been pruned | as above |
| `carries no build.yml signature for <sha>` | the `sha-` tag now names a digest some other commit's build pushed (a 7-character collision), or the image is unsigned | investigate before releasing; never re-sign by hand |
| `has no SBOM / Provenance attestation` | the digest was not pushed by `build.yml` | as above |
| `<repo>:<tag> resolves to <x>, not the promoted <y>` | the retag did not preserve the digest | do not deploy the tag; pin `@<digest>` from the manifest |

The tag-time `release.yml` run is idempotent: re-running it re-resolves the
same digests, re-points the same tags and adds another release signature.

The shell is a remote WebView onto whichever deployed stack the user picks
on-device at first launch, so a server or PWA release reaches installed
phones without a new APK. The shell's lifecycle/connectivity/voice
validation — the emulator instrumentation suite and the device / Play
pre-launch checklists — is in
[`mobile-release-validation.md`](mobile-release-validation.md).

Publishing is not deploying: a release changes nothing you run until you pin
its digests (§6). Because a release digest *is* a `main` build digest, a
deployment already running the `sha-` build of the tagged commit is already
running the release; only its pin's spelling changes.

Pushes to `main` are **builds, not releases**. `build.yml` publishes the same
three images tagged `sha-<commit>` (signed, with SBOM and provenance) and the
demo image as `main` and `main-<commit>`. No semver tag is created and
`latest` does not move, so "which build is that?" stays answerable. Pin the
release family for a deployment; a `sha-` image is a way to run a specific
commit, not a version. One visible consequence of promoting a `main` build:
the stack's build metadata names `main` as its source ref (with the tagged
commit's sha and the release version), not the `vX.Y.Z` tag.
