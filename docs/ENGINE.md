# Vogt — The Session Engine

This document describes what the engine *is*. Outstanding engine work is
tracked in the GitHub issue tracker, not here.

The session engine is the Rust half of Vogt — its execution surface. It runs
PTYs, streams them over WebSocket, serves the PWA, and is the
merged product's **front door**: the only listening process, proxying
`/api/vogt` and `/mcp` to the Python core on loopback.

**The engine is optional.** The Python core (the repository-root `Dockerfile`,
published as `ghcr.io/thedancingdeveloper-org/vogt`) is a complete product on
its own. The engine, the PWA under `web/` and the Android shell under
`mobile/` are built from source with `engine/Dockerfile` — §3 and
[`DEPLOYMENT.md`](DEPLOYMENT.md) cover both paths.

This file is the single reference for the engine: what it owns, how to run
it, the full wire contract, the assistant and its threat model, agent tasks.

Companion documents: [`ARCHITECTURE.md`](ARCHITECTURE.md) (the product's architecture),
[`DEPLOYMENT.md`](DEPLOYMENT.md) (production deployment: images, compose,
env, reverse proxy, backups, upgrades), [`USER_GUIDE.md`](USER_GUIDE.md) (how
a person drives it).

---

## 1. What the engine owns

Vogt's Python core owns the *work* — projects, backlog, bugs, ranking, drift,
contract, audit, and the operation registry every surface is generated from.
The engine owns the *doing*:

- **Terminal sessions.** A PTY per session, server-owned so it survives every
  client disconnecting, with a ring-buffer scrollback (4 MiB default) and
  WebSocket attach that replays a snapshot before live output.
- **Activity state.** Each session carries `idle` / `running` /
  `waiting-for-input` / `errored`, derived from output heuristics and published
  on a server-wide SSE stream.
- **Hibernation.** An agent session can have its process tree stopped to free
  memory and be woken later, under the same id, by resuming its conversation.
  The record that makes this possible is written at spawn, so sessions also
  survive an engine restart ([Hibernation](#hibernation)).
- **Agent tasks.** A durable scheduled-agent registry — `manual`, `interval`,
  UTC `daily`, plus **event triggers** that fire runs from vogt-core's own
  state (a work-item transition, a raised drift proposal, a new observation, a
  PR's checks flipping) and an explicit `api` fire — whose runs are real PTY
  sessions, optionally bound to a Vogt project or work item.
- **The assistant.** A server-side tool-use loop over sessions and a curated
  read slice of Vogt, with every effector behind an on-screen approval.
- **Workspace-scoped file and git APIs**, a GUI process launcher, web push
  (VAPID and FCM), and archived session history.
- **The front door.** The single published port: the PWA, the engine's native
  APIs, the WebSocket attach path, `/api/vogt` and `/mcp` proxied to the core,
  and aggregate health.

Two properties hold the merged shape together and are asserted rather than
described. **The core is the only definition of an operation**: the
PWA's route table resolves against the registry, and the assistant's Vogt tools
are fetched from the core's own MCP `tools/list` rather than written out again.
**The engine is bootable alone**: with no core configured it serves
sessions, stays ready, and refuses the Vogt routes with a named reason —
`engine/server/tests/vogt_core.rs` is what says so.

## 2. The tree

```
engine/           its own Cargo workspace — the repository root is not one
  server/         vogt-engine-server crate → `vogt-engine` binary: PTYs, HTTP, WS, SSE, the front door
  contract/       vogt-engine-contract: shared wire DTOs
  Dockerfile      the merged image (context is the repository root)
  Dockerfile.pod  the dev-pod toolchain base the merged image builds on
  deploy/         entrypoint, MCP registration and credential helpers, the
                  runtime agent-CLI installer
web/              the Solid/Vite PWA — the product's GUI, embedded at build time
mobile/           the Capacitor 8 Android shell that loads the deployed PWA
```

The engine's checks run in `.github/workflows/ci.yml` and its image is built
by `.github/workflows/build.yml`.

Engine settings are read under the `ENGINE_*` environment prefix (see §3);
the helper binaries the image installs are named `vogt-*` (§3.1); the
variables a session inherits are `VOGT_ENGINE_*` (§5, §7).

`engine/` is its own Cargo workspace, so every `cargo` invocation runs from
`engine/`. The Rust binary embeds the repository-root `web/dist/` via
`rust-embed` at compile time, which means **a `cargo build` without a fresh
`pnpm build` ships a stale frontend** — the most common way to fix a UI bug and
see nothing change.

## 3. Running it

Two ways: build the merged image, or run the binary from source. Both need
the PWA built first (§3.3), because the binary embeds it.

### 3.1 Toolchain and the merged image

From source you need a stable Rust toolchain, Node 22 with `pnpm`, and
`ripgrep` on `$PATH` (the search routes shell out to `rg`). `git` is needed for
the git routes.

The merged image is built from the **repository root**, not from `engine/`:

```bash
docker build -f engine/Dockerfile \
  --build-arg CORE_IMAGE=ghcr.io/thedancingdeveloper-org/vogt:latest \
  -t vogt-engine .
```

The Dockerfile's stages are the PWA bundle, the Rust binary with that bundle
embedded, the published core image (lifted whole, so the merged image runs
*the* public core rather than a second build of it — `CORE_IMAGE` defaults to
the public `:latest` tag as a placeholder for a local build, and CI overrides
it with a digest), and the runtime. The runtime stage starts `FROM` a
**dev-pod base** (`engine/Dockerfile.pod`, published as
`ghcr.io/thedancingdeveloper-org/vogt-pod-base:lean-*`): an Ubuntu image
carrying the toolchains an agent working *inside* a session is likely to want
— Node, Rust, Java/Gradle, the Android SDK; its `INSTALL_FLUTTER` build
argument adds Flutter for a pod that builds the Android app. It is a
development pod rather than a hardened service image — it runs as a named
user with `sudo`, with a writable home — which is why the core image at the
repository root is a separate build rather than a stage of this one: it is
the hardened input this image lifts its core from, not something to deploy on
its own (`DEPLOYMENT.md`).

Build arguments worth knowing: `INSTALL_AI_CLIENTS=true` bakes in the `codex`,
`claude` and `opencode` CLIs at the Renovate-pinned versions in
`engine/agent-versions.env`, and `klaudia` (WI-950), a Go coding agent no
registry publishes, built with the image's Go from the commit pinned there
(`VOGT_KLAUDIA_VERSION`, a full commit id of `VOGT_KLAUDIA_SOURCE`); it is off by default for a local build and on in
the published image. That baked copy is the baseline, and the version a
running pod uses can be moved without a rebuild by the runtime pin described
in [`DEPLOYMENT.md`](DEPLOYMENT.md) §3 (`VOGT_CLAUDE_CODE_VERSION` and
friends, applied by `vogt-agent-cli-install` at container start).
The image also carries a Go toolchain (WI-951), unconditionally: the release
`engine/agent-versions.env` pins as `VOGT_GO_VERSION`, checked against the
sha256s recorded beside it, unpacked to `/usr/local/go` and linked as
`/usr/local/bin/go` and `gofmt`, with `GOTOOLCHAIN=local` so a `go.mod` asking
for a newer toolchain fails by name rather than fetching one. It is a row of
the same runtime-pin table (kind `go-dist`), so `VOGT_GO_VERSION` in the
environment moves it at the next start without a rebuild, checked against
the Go mirror's own release index. Klaudia's row is kind `go-src`: a runtime
`VOGT_KLAUDIA_VERSION` is another full commit, fetched alone and built into
the volume at start (`./cmd/klaudia`, CGO off), its `source` recorded in the
prefix. `GO_VERSION` as a build argument must
equal the pin: a build cannot verify a version it has no recorded checksum
for.
`POD_BASE_IMAGE` selects the pod base; CI passes it by digest.

The image's entrypoint (`engine/deploy/entrypoint.sh`) supervises the
container: it optionally starts a headless compositor for the GUI surface
(`START_SWAY=1`), starts the Python core on loopback when `VOGT_CORE_URL`
names a loopback address, then execs the engine as PID 1's child. With
`VOGT_CORE_URL` unset the container runs the engine alone. The published
image runs with `deploy/stack.compose.yml`; `deploy/engine.overlay.yml`
builds the engine from source in front of the public core image for
contributors. Neither carries host paths or secrets-manager assumptions — every
deployment-specific value is a `${VAR}` placeholder. [`DEPLOYMENT.md`](DEPLOYMENT.md)
is the production guide.

The helper scripts the image installs under `/usr/local/bin/vogt-*`:

| Helper | Source | What it does | Needed? |
|---|---|---|---|
| `vogt-entrypoint` | `deploy/entrypoint.sh` | container supervisor, above | yes |
| `vogt-agent-cli-install` | `deploy/agent-cli-install.sh` | applies the runtime agent-CLI pin at boot and on `POST /api/agent-clis/{tool}` (§5) | only with agent CLIs present |
| `vogt-mcp-bootstrap` | `deploy/mcp-bootstrap.sh` | registers Vogt's MCP server with the agent CLIs present in the image, and the read-only third-party servers whose tokens the session holds (§4) | optional — without it an agent registers MCP servers by hand |
| `vogt-mcp` | `deploy/vogt-mcp-auth.sh` | stdio bridge to Vogt's `/mcp` for clients that cannot take a bearer directly; uses the session's own token | optional |
| `vogt-rust-analyzer-mcp` | `deploy/rust-analyzer-mcp.sh` | starts `rust-analyzer-mcp` anchored to the nearest `Cargo.toml` | optional |
| `vogt-readonly-mcp` | `deploy/readonly-mcp.sh` | starts the GitHub, Grafana or Gitea/Forgejo MCP server in read-only mode with the session's token (§4) | optional — registered only when its token is present |
| `vogt-klaudia-mcp` | `deploy/klaudia-mcp.sh` | `set [-e KEY=VALUE]…`/`remove` a stdio server in Klaudia's `.mcp.json` (`~/.klaudia`, or `KLAUDIA_CONFIG_DIR`), which Klaudia has no `mcp` command to write; `vogt-mcp-bootstrap` uses it, and a derivative image's bootstrap may (§4) | only with `klaudia` present |
| `git-forgejo` | `deploy/git-forgejo.sh` | git with a Gitea/Forgejo token header that cannot be word-split (§4) | optional |
| `vogt-git-askpass` | `deploy/git-askpass.sh` | `GIT_ASKPASS` shim for brokered credentials | optional |
| `codex` wrapper | `deploy/codex-full-access.sh` | runs Codex without its nested sandbox, because the pod is the isolation boundary | only with `INSTALL_AI_CLIENTS` |
| `vogt-agent-auth` | `deploy/agent-auth.sh` | reference `ENGINE_AGENT_AUTH_HELPER`: brokers service credentials from a secrets manager into a session (`check`, `run -- <cmd>`, `shell`, `fetch`, `store`), driven entirely by an env manifest with no baked addresses or secret names | **optional and pluggable** — one example helper; see §9 |

Nothing in the engine itself depends on `agent-auth`: with
`ENGINE_AUTO_AGENT_AUTH` unset (the default) sessions are plain shells, and
the three "(protected)" session templates that wrap an agent CLI in it
(Claude Code, Codex, OpenCode) simply fail to start if the helper has nothing
to broker.

### 3.2 From source

```bash
# 1. Choose how callers are authenticated. Fronting a core (VOGT_CORE_URL),
#    credentials are the core's — password-login sessions and API tokens, §5 —
#    and nothing is needed here. Standing alone there is no core to ask, so a
#    static break-glass token (>=16 chars) is the only way in and the engine
#    refuses to boot without one.
export ENGINE_TOKEN="$(openssl rand -hex 24)"
# Optional: the per-identity write-rate cap (600 a minute by default).
export ENGINE_MUTATING_REQUEST_LIMIT_PER_MINUTE=600

# 2. Run — from engine/, which is the Cargo workspace root
cd engine
cargo run -p vogt-engine-server -- --bind 127.0.0.1:8910
```

Engine settings are read from `ENGINE_*` environment variables. That includes
the three values the CLI parser owns — the token, the bind address and the
config path: they read `ENGINE_TOKEN`, `ENGINE_BIND` and `ENGINE_CONFIG`, or
the `--token` / `--bind` / `--config` flags, which win over the environment.
Prefer the environment for the token so it does not appear in process
listings.

Optional TOML config, passed with `--config engine.toml`. Precedence is
CLI flags > env > config file:

```toml
bind = "0.0.0.0:8910"
token = "..."                  # or ENGINE_TOKEN; optional break-glass credential
vogt_core_url = "http://127.0.0.1:8000"                 # or VOGT_CORE_URL
vogt_core_token_file = "/run/secrets/vogt_core_token"   # or VOGT_CORE_TOKEN(_FILE); the stack secret
scrollback_bytes = 4194304
default_shell = "/bin/bash"
default_cwd   = "/srv/workspace"
workspace_root = "/srv/workspace"      # default: ~/Working; `/`, $HOME or a root holding state_dir fails the start
activity_idle_after_ms = 1500
state_dir = "/var/lib/vogt-engine"     # default: ~/.local/share/vogt-engine
vapid_subject = "mailto:admin@example.invalid"
allowed_origins = [
  "https://vogt.example.com",
  "http://localhost:5173",
  "http://127.0.0.1:5173",
]
auto_agent_auth = false
agent_auth_helper = "/usr/local/bin/vogt-agent-auth"
token_mutating_request_limit_per_minute = 600
```

The engine keeps no token table. `token` is the optional static
**break-glass** credential: full capability, no actor of its own, and its
Vogt calls are attributed to the stack secret's actor. Every other bearer is
a **core token** — a person's session from a password login, or an agent's
API token — which the engine resolves by asking the core (§5, "Core rules"),
so a deployment whose people all have a login leaves `token` unset and every
credential is checked in one place. The PWA's Settings modal still stores
device-local named auth profiles for token sign-ins; a password login needs
none of that.

To run the engine as the front door for a core, set `VOGT_CORE_URL` to the
core's address and `VOGT_CORE_TOKEN` (or `VOGT_CORE_TOKEN_FILE`) to the
**stack secret** — the file the core also reads as
`VOGT_BOOTSTRAP_CORE_TOKEN_FILE` and adopts at `init`. The engine recognises
that value as the core's own identity when the core calls in to start a
session, follows the core's event feed with it, and lends it to the
break-glass token; it is never presented on behalf of anybody else. With
neither `VOGT_CORE_URL` nor `ENGINE_TOKEN` set the engine refuses to start,
because there would be no way to authenticate anybody. The assistant is
configured in §6.

Which values the *core* reads is [`CONFIG.md`](CONFIG.md), which is generated
from `src/vogt/config.py` and does not describe this process.

### 3.3 Refreshing the embedded PWA

```bash
cd web && pnpm install && pnpm build
cd ../engine && cargo build --release
```

For UI work, run the server and the Vite dev server in parallel — Vite proxies
`/api` and the WebSocket endpoint to the backend:

```bash
# terminal 1
cd engine
ENGINE_TOKEN=$(openssl rand -hex 24) cargo run -p vogt-engine-server -- --bind 127.0.0.1:8910
# terminal 2
cd web && pnpm dev   # -> http://127.0.0.1:5173; "Sign in with a token" under the password form
```

### 3.4 Tests

```bash
cd engine                        # the Cargo workspace root
cargo fmt --check
cargo clippy -- -D warnings
cargo test                       # server unit + integration (HTTP + WS)
cargo test -p vogt-engine-contract # shared wire-contract tests

cd ../web && pnpm typecheck      # PWA TypeScript check
cd ../web && pnpm test           # jsdom tests over the PWA's Vogt surfaces
```

The Python core's suite runs from the repository root and does not need either
toolchain; CI runs it with `engine/`, `web/` and `mobile/` deleted to prove
it.

### 3.5 Smoke test with curl + websocat

```bash
TOKEN=$ENGINE_TOKEN
BASE=http://127.0.0.1:8910

curl -s $BASE/healthz
curl -s $BASE/readyz

ID=$(curl -s -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
     -d '{"name":"shell-1"}' $BASE/api/sessions | jq -r .id)
curl -s -H "Authorization: Bearer $TOKEN" $BASE/api/sessions | jq

# Attach over WebSocket. The first frame must authenticate:
websocat "ws://127.0.0.1:8910/api/sessions/$ID/attach"
#   {"type":"auth","token":"'"$TOKEN"'","resume_from":123}
#   {"type":"resize","cols":120,"rows":40}

curl -sN -H "Authorization: Bearer $TOKEN" $BASE/api/events     # SSE
curl -s -X POST -H "Authorization: Bearer $TOKEN" $BASE/api/sessions/$ID/kill
curl -s -X DELETE -H "Authorization: Bearer $TOKEN" $BASE/api/sessions/$ID
```

The merged stack has its own smoke script, `scripts/smoke_merged_stack.sh`,
which checks the front door end to end. It exists because the failure worth
catching is not a crash but a front door that comes up, passes its healthcheck
and serves no Vogt.

## 4. Agent-facing MCP servers inside the pod

The runtime image bundles MCP servers so agents running in a session can
reach a Rust LSP and Vogt itself without the user wiring anything, and —
when a deployment hands sessions the token — GitHub, Grafana and a
Gitea/Forgejo forge, read-only.
Everything in this section is about the *image*; a from-source engine has
none of it and loses nothing the engine's own routes provide.

**Rust LSP.** `vogt-rust-analyzer-mcp` wraps `rust-analyzer-mcp` and
starts it from the nearest parent directory containing a `Cargo.toml`, which
keeps the analyzer anchored to the active workspace when an agent launches it
from a subdirectory.

```bash
codex mcp add rust-analyzer -- vogt-rust-analyzer-mcp
claude mcp add --scope project rust-analyzer -- vogt-rust-analyzer-mcp
```

Set `VOGT_ENGINE_RUST_ANALYZER_WORKSPACE` for that MCP entry if a client launches
it from a non-Rust directory.

**GitHub, Grafana, Gitea/Forgejo — optional, read-only.** The image carries
three third-party MCP servers at exact versions, each tarball checked against a
sha256 recorded in `engine/Dockerfile` (a bump is an edit there):
`github-mcp-server` 1.14.0, `mcp-grafana` 2.0.0 and `gitea-mcp` 1.8.0.
`vogt-mcp-bootstrap` registers each one with Claude Code, Codex, opencode
and Klaudia **only while the session holds its token**, and removes the
registration when it does not (opencode excepted: it has no `mcp remove`, so
its entry stays and the wrapper, finding no token, refuses to start), so a deployment turns one on by adding its token to the session's
environment (normally a launch-time line in `ENGINE_AGENT_AUTH_SECRETS`, §9 —
an `ondemand` line is not in the environment when the bootstrap runs, so it
does not register anything) and off by removing it.

| Registered as | Turned on by | Read-only because |
|---|---|---|
| `github-ro` | `GITHUB_MCP_TOKEN` — a fine-grained, read-only PAT; `VOGT_GITHUB_MCP_TOOLSETS` overrides the toolsets (default `actions,pull_requests,repos,code_security,dependabot`) | `--read-only` and `GITHUB_READ_ONLY=1` (write tools are not offered), plus the token's scope |
| `grafana-ro` | `GRAFANA_URL` + `GRAFANA_SERVICE_ACCOUNT_TOKEN` — a Viewer-role service account | `--disable-write` (create/update tools and raw-SQL query tools are not offered), plus the Viewer role; `--usage-stats=disabled` |
| `forgejo-ro` | `GITEA_HOST` + `GITEA_MCP_TOKEN` — a read-scoped token; works against Forgejo's Gitea-compatible API | `-r` and `GITEA_READONLY=true` (only read tools are offered), plus the token's scope |

The registration stores `vogt-readonly-mcp <server>` and nothing else: no
token value (Claude Code, opencode and Klaudia pass a stdio server the
session's environment; Codex is told which variables to pass with `env_vars`) and no flag a session
could edit. `vogt-readonly-mcp` (`deploy/readonly-mcp.sh`) renames the token
to the variable the upstream server reads and sets the read-only switches on
every start, overriding any `GITHUB_TOOLSETS`, `GITHUB_READ_ONLY` or
`GITEA_READONLY` the session carries. The `-ro` names leave a server an
operator registered by hand under its plain name untouched. Read-only rests
on the token as well as on the server's mode, so each token should be issued
read-only even though the mode already hides write tools.

Cloudflare is not among them. Cloudflare's official MCP servers are hosted
(`*.mcp.cloudflare.com`), so there is no version to pin, and none has a
read-only mode — only the API token's scopes would make one read-only.

**`git-forgejo`.** For git itself against a Gitea/Forgejo host, the image
installs `git-forgejo` on PATH (so `git forgejo …` works too). It reads
`FORGEJO_TOKEN` and `FORGEJO_URL` (falling back to `GITEA_HOST`), and hands git
`http.<FORGEJO_URL>/.extraheader=Authorization: token …` through
`GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n` — one value no shell
re-splits, scoped to the forge so a submodule or redirect elsewhere is not
handed the token, and never on git's command line where `/proc/<pid>/cmdline`
shows it. The hand-written `git -c http.extraheader="Authorization: token
$TOKEN"` breaks the moment a layer of shell drops its quotes.

**Vogt.** A session started for a project or work item registers Vogt's own MCP
server automatically, carrying a per-session actor-scoped token, so an agent's
writes are attributed to that session's actor rather than to a shared
identity. The session exports the endpoint as `VOGT_URL` and
`mcp-bootstrap.sh` registers it with every agent CLI in the image — Claude
Code (`~/.claude.json`), Codex (`~/.codex/config.toml`), opencode (its user
`opencode.json[c]`) and Klaudia (`~/.klaudia/.mcp.json`, through
`vogt-klaudia-mcp`); `VOGT_MCP_URL` overrides the endpoint, and
with neither set the bootstrap falls back to the front door on loopback.

Any further MCP server an agent should reach is registered by hand inside the
session, the way the Rust LSP entry above shows.

---

## 5. The wire contract

This section is the source-of-truth summary for the engine's wire contract.

`engine/server/src/app.rs` holds the route table and is what actually answers
requests; this section describes it route by route. `tests/test_pwa.py` resolves
every engine path in the shipped PWA against both, so the two cannot drift
apart silently. When they do disagree, this section is the one that is wrong.

### Contract crate

Rust DTOs live in `engine/contract/` (`vogt-engine-contract`); the server is
their only consumer in this tree. Those types cover:

- session lifecycle payloads
- SSE event payloads
- file and git API payloads
- WebSocket attach control frames
- common small response shapes like `{"ok": true}`

The browser client carries TypeScript mirrors in `web/src/api.ts`; those
shapes follow the shared Rust contract rather than ad hoc server-local
structs. The browser/PWA is the supported client surface.

Routes whose response shape is named below without a crate to look it up in are
server-local: the handler's own struct in `engine/server/src/`, named in the
section that documents it.

### Core rules

- All `/api/*` HTTP routes require bearer auth except `/api/config`,
  `/api/push/public-key`, and the pass-throughs the core gates itself —
  `POST /api/auth/login` and `/api/install/*` (as `/mcp` is, outside `/api`).
- `GET /healthz` and `GET /readyz` are public.
- Auth is `Authorization: Bearer <token>`. The engine holds no token table.
  A bearer is resolved in order of cost (`authorize` in
  `engine/server/src/auth.rs`): the optional static break-glass
  `ENGINE_TOKEN`, constant-time; the stack secret (`vogt_core_token`),
  constant-time — the core's own identity when it calls this engine; and
  otherwise a **core token**, which the core is asked about
  (`GET /api/auth/whoami` with the bearer forwarded,
  `engine/server/src/core_auth.rs`). That answer is cached by the SHA-256
  digest of the bearer — 15 s for an identity, 3 s for a refusal, never for
  an outage — and a successful `POST /api/vogt/auth/logout` evicts its entry,
  so a signed-out session stops opening engine routes at once rather than at
  the TTL. There is no fourth kind of credential.
- **A resolved bearer is enough for most routes.** Some also require a named
  capability, and those say so; where nothing is said, any resolved bearer
  will do. The capabilities are `sessions`, `filesystem-write`, `git-write`,
  `gui-control`, `agent-tasks-write`, `push-write`, `history-write`, `history`,
  `assistant`, `vogt-write` and `agent-clis-write` (`sessions` also gates
  reading a session's detail and scrollback, and every read of the workspace
  tree — files, downloads, listings, the tree, both searches and the git
  reads; `history` gates reading archived session history — all reads, gated
  because they expose other callers' output or work). They derive from core scopes (`capabilities_for_scopes`): `admin`
  holds all eleven; `work.write` or `project.write` holds everything except
  `gui-control` and `agent-clis-write`; `read` alone holds `push-write`, so a
  viewer can subscribe to notifications; `writeback` adds nothing. The
  break-glass token holds all eleven, and the stack secret holds `sessions`,
  `history` and `agent-clis-write` — what the core needs to start sessions
  here and to read the archive of the ones that ended. The
  capability-to-route mapping lives in `required_capability` in
  `engine/server/src/auth.rs` and is keyed on method *and* path, so
  `GET /api/sessions` needs no capability while `POST /api/sessions` needs
  `sessions`.
- A bearer that resolves but lacks the capability gets `403`, not `401`; a
  bearer nobody recognises gets `401`; and a bearer the core could not be
  asked about gets **`503`** with `Retry-After`, never `401` — telling a
  browser its credential is wrong because the core is restarting is exactly
  the collapse of "offline" into "unauthorized" the PWA guards against. The
  distinction is worth acting on: `401` means try a different credential,
  `403` means this credential will never work for this route, `503` means
  wait.
- **`gui-control` and `agent-tasks-write` are arbitrary code execution**, equal
  in power to `sessions`: `gui/launch` runs any argv and an agent task runs a
  caller-supplied `command` in a PTY, both as the pod user (which holds
  `NOPASSWD:ALL` sudo). Grant them only where you would grant a shell; do not
  read them as narrower than they are.
- Every mutating request (POST/PUT/PATCH/DELETE) is rate limited per
  identity — 600 per minute by default
  (`token_mutating_request_limit_per_minute`), keyed by the resolved name.
  Over the limit is `429` with `Retry-After` in whole seconds.
- **Every** response carries `X-Request-Id`, gated or not, adopted from the
  request when it sent a usable identifier and minted otherwise. One
  request has exactly one id: the outermost layer assigns it, the audit lines
  for mutating requests use it, the access line below quotes it, and
  `/api/vogt` and `/mcp` send it to the core, which attaches it to every line
  it writes while serving that request. That is what makes a slow request
  followable across two runtimes with two logging stacks. A caller-supplied id is checked before it is logged (identifier
  characters, 64 bytes) and replaced when it is not, and the audit lines still
  carry the token's *name* and never its value.
- Every request produces one `tracing` line: `request_id`, `method`, `path`,
  `status`, `duration_ms`. Slower than a second or a `5xx` logs at `warn`;
  probes and `/assets/*` log at `debug`, so a page load does not bury the one
  line worth reading. Verbosity is `RUST_LOG`'s
  (`engine/server/src/observability.rs`).
- Errors are `{"error": "<message>"}` with the status on the HTTP line: `400`
  malformed or out-of-bounds input, `401` no or wrong token, `403` capability
  denied, `404` not found (also: feature not provisioned, see below), `409`
  conflict, `429` rate limited, `500` something the engine owns is broken,
  `502` an optional upstream did not answer, `503` the core could not be
  asked about a credential. The gate's refusals use the same shape:
  `unauthorized: no bearer token`, `unauthorized: the bearer token is not
  valid here`, `forbidden: <who the credential is>; it lacks the <X>
  capability, which <scope> grants`, and `vogt-core is unavailable, so this
  credential cannot be checked: <detail>`. The `403` names the identity the
  bearer resolved to and its scopes, or says it is the stack secret — which
  holds `sessions` but never `vogt-write`, so a process holding it can type
  into sessions but cannot `POST /api/vogt/sessions`. A pod session's own
  `VOGT_HTTP_TOKEN` is a core token with `agent_session_scopes` (by default
  `work.write`, so `vogt-write`); a `403` on a Vogt write from inside a session
  means the variable holds some other credential (WI-871).
- A feature that is not provisioned answers `404` rather than `501` or `503`:
  the assistant with no API key. The feature is invisible rather than
  advertised-but-broken. The Vogt front
  door is the exception and answers `503` with a named reason, because an
  absent core is an outage of something that exists rather than a feature that
  was never turned on.
- WebSocket attach authenticates with the first text frame:
  `{"type":"auth","token":"..."}`
- WebSocket PTY traffic is binary. Text frames are control messages only.
- CORS allows GET/POST/PUT/PATCH/DELETE/OPTIONS with `Authorization`,
  `Content-Type` and `Accept`, credentials off, preflight cached 600s. With no
  `allowed_origins` configured no `Access-Control-*` headers are emitted at
  all — same-origin only, which is how the embedded PWA is served.

### Public routes

- `GET /healthz` -> `OkResponse` — the process is listening. It reads nothing,
  so it stays cheap enough for a container liveness probe.
- `GET /readyz` -> `{"ok": bool, "checks": [{"name","ok","detail","fatal"}]}`
  — six checks: `workspace_root` (readable directory), `state_dir`
  (writable, proved by writing and removing a probe file), `gui` (skipped
  unless a GUI stream is configured), `vogt_core`, `workspace_agreement` (the
  core imports inside this server's workspace root) and `backup_agreement`
  (`vogt backup` would cover this server's `state_dir`). `200` when every
  *fatal* check passes, `503` otherwise; the last three are non-fatal by
  design, so a ready container can still be reporting one of them false.
- `GET /api/config` -> `PublicConfig`
  `{gui_stream_url, gui_stream_available, version, product_version,
  source_ref, source_sha, release_url, features, session_templates,
  assistant_enabled, assistant_stt_enabled, assistant_tts_enabled,
  assistant_call_enabled, vogt, assistant_model?, assistant_profiles?}` — what the browser needs at boot,
  before the user has typed a token into Settings. It is outside the gate
  because it returns no secrets: `assistant_enabled` is presence only, never
  the key.
  `features` is read per request from `/etc/vogt/features.json` and is
  `{}` when that file is absent. `gui_stream_available` is the server-owned
  UI gate and is true only when a stream URL, a non-null Selkies feature and
  `GUI_STREAM_VERIFIED=1` are all present. The latter is an operator
  attestation set only after a launched process has rendered through the
  configured stream.
- `GET /api/auth/check` -> `{ok, version?, product_version?, storage?,
  identity: {name, scopes, capabilities}}` — a cheap authenticated credential
  probe. `identity.name` is the core actor's `identity_ref` (`human:ada`,
  `agent:session:…`), or `primary` / `vogt-core` for the two static
  credentials, whose `scopes` are empty. It deliberately does not perform the
  operational checks reported by `/api/status`, so Settings can distinguish a
  valid credential from a temporarily unavailable engine without starting a
  full status read.
- `POST /api/auth/login` `{username, password, session_name?}` ->
  `{actor, token, secret}` — the password login, forwarded untouched to the
  core. Open because a browser that holds no session yet is exactly its
  caller; the core owns the credential check, the per-username throttle
  (`429 login_throttled` after five failures in a minute) and the audit row.
  See "The Vogt front door" below.
- `GET /api/push/public-key` -> `{vapid_public_key, fcm_enabled}` — needed to
  call `PushManager.subscribe`, so it must be reachable before a token exists.

`vogt_core` is the only non-fatal readiness check. The core is a separate
process with its own lifecycle: restarting this container would not revive it
and would kill every live PTY, which is exactly what an absent core must not
cost. So its state is reported in full and left out of the verdict, and the
surfaces that need it say so themselves.

### Session APIs

The machine-readable contract for these routes is
[`engine-openapi.yaml`](engine-openapi.yaml) (OpenAPI 3.1, hand-maintained;
`scripts/check_engine_spec.py` fails CI when a `/api/sessions…` route in
`engine/server/src/app.rs` has no path there). This section is the prose:
what the fields mean, and how to drive a session safely.

**Agents drive sessions through the core first.** vogt-core wraps these
routes as registry operations on CLI, REST and MCP — `session.start`,
`session.list`, `session.screen`, `session.input`, `session.log_tail`,
`session.stop` (MCP `session_start` … `session_stop`). They accept either id
form, and every `session.input` is audited. Calling the engine directly is
the fallback for what they do not wrap. The recipe is in
[Driving a session](#driving-a-session) below.

#### Reaching the engine from a session

Every session the engine spawns has `VOGT_ENGINE_URL` in its environment: the
engine's own bind address, with a wildcard bind (`0.0.0.0`, `::`) read as
loopback, so `http://127.0.0.1:8910` in the shipped stack. A session vogt-core
starts gets the URL the core reaches the engine at instead (in the merged
stack, the same loopback address), because a caller's `env` overrides the
engine's default. An engine bound to port `0` cannot name its port, and sets
nothing.

Authenticate with `Authorization: Bearer $VOGT_HTTP_TOKEN`. In a session the
core started that is the session's own token, minted with
`agent_session_scopes` (by default everything except `admin`). It includes
`work.write`, which the engine maps to the `sessions` capability, so it can
create, read, type into and stop sessions (§5 "Core rules";
`GET /api/auth/check` lists a bearer's `capabilities`). A session opened from
the GUI has a `VOGT_HTTP_TOKEN` only when the deployment's agent-auth
manifest brokers one, and then it is the pod's token, not one attributed to
that session.

#### Session ids

A session has up to two ids:

| Id | Example | Who issues it | Accepted by |
|---|---|---|---|
| Engine session UUID | `0b0e5f0c-1111-4222-8333-944455556666` | the engine, for every session | every engine route (`{id}` below); every core `session.*` operation |
| Vogt session id | `ses_01M42…` | vogt-core, for sessions it started (`session.start`) | core `session.*` operations only |

`session.list` shows both (`id` and `engine_session_id`). A session opened
from the GUI is *unlinked*: it has only the UUID, and `linked` is false. The
engine routes take only the UUID; a `ses_…` id in the path is a `400`. The
session's own child sees its engine UUID as `VOGT_ENGINE_SESSION_ID`, and a
session the core started also sees its Vogt id as `VOGT_SESSION_ID`.

#### Routes

Each route below has a core operation that is its MCP counterpart, or a
stated reason it has none, in `src/vogt/registry/engine_routes.py`
(`tests/test_engine_parity.py` enforces it; `API.md` has the table). An agent
should use those operations — they take either id and are audited — rather
than these routes.

- `GET /api/sessions` -> `SessionSummary[]` — any valid bearer. Exited
  sessions are included until deleted; filter on `alive`.
- `POST /api/sessions` `SessionSpec` -> `SessionSummary` (requires the
  `sessions` capability)
- `GET /api/sessions/:id[?tail_bytes=N]` -> `SessionDetail` — the raw
  scrollback, base64. Requires the `sessions` capability, because scrollback
  routinely holds pasted secrets.
- `GET /api/sessions/:id/screen[?scrollback_lines=N]` -> `SessionScreen` —
  the current terminal screen, rendered, plus up to N (≤ 2000) lines that
  scrolled off its top (see [Reading the screen](#reading-the-screen)). Gated
  like `GET /api/sessions/:id`: the `sessions` capability
- `GET /api/sessions/:id/wait[?until=ready|exited|any-change&timeout_s=N]`
  -> `SessionWait` — blocks on the event bus until the session is ready (or
  needs a person, or exits), exits, or changes, or `timeout_s` (default 120,
  at most 600) passes; answers with the `outcome` (`ready`,
  `awaiting-approval`, `blocked`, `exited`, `changed`, `timeout`), whether
  it `matched`, `waited_ms` and the `screen` then. Requires `sessions`.
- `POST /api/sessions/:id/blocked` `SetBlocked` -> `SessionSummary` — set
  (`{"blocked": true, "reason", "items"}`) or clear (`{"blocked": false}`)
  the agent's "blocked on a person" report, which rides on the summary and
  screen as `blocked`, publishes a `session-blocked` event and (when set)
  sends a push. Cleared when the session exits. Requires `sessions`.
- `PATCH /api/sessions/:id` `{"name": "..."}` -> `OkResponse` (requires the
  `sessions` capability). MCP: `session_rename`.
- `POST /api/sessions/:id/kill` `{"reason"?, "by"?}` -> `OkResponse`
  (SIGKILL to the child; the session stays in the registry so its
  scrollback is still readable, which is what makes this different from
  `DELETE`. The optional body is recorded before the kill, and the exit then
  reads `stopped` rather than `errored`. Requires the `sessions`
  capability)
- `POST /api/sessions/:id/input` `{"text": "...", "submit": bool, "person"?}`
  -> `OkResponse` (writes verbatim to PTY stdin, 64 KiB cap, `submit`
  appends `\r`; requires the `sessions` capability). `403 person required`,
  with nothing typed, when a permission prompt is showing and the caller is
  not a person ([Only a person answers a permission
  prompt](#only-a-person-answers-a-permission-prompt)).
- `DELETE /api/sessions/:id` -> `OkResponse` — kills the child if it is still
  running, then forgets the session and its prompt file (requires the
  `sessions` capability). MCP: `session_remove`.
- `GET /api/sessions/:id/attach` — the WebSocket stream (see
  [Attach protocol](#attach-protocol)); a driver does not need it.
- `POST /api/sessions/:id/answer` `{"option": N | "label": "...",
  "expect_question"?, "person"?}` -> `AnswerResult` — choose an option of the dialog on
  screen: a permission dialog, or a startup gate (`approval.kind`
  `folder-trust`, `external-imports`, `read-outside-cwd`). The engine
  re-reads the menu at that moment, moves the highlight from where it is
  with arrow keys (one write each), presses Enter, and looks again for up to
  2 s to report `dismissed`. `409` when no dialog is showing, when it no
  longer asks `expect_question`, when the option is not on the menu, or when
  a label matches several options. Requires `sessions` (WI-917). A
  permission prompt (`kind` `permission` or `read-outside-cwd`) is answered
  only by a person: anyone else gets `403 person required` and nothing is
  typed (WI-983, below).
- `GET /api/sessions/sweep[?screen_lines=N&include_exited=true]` ->
  `SessionSweepEntry[]` — every live and hibernated session (exited ones
  only when asked) with the last N (default 8, at most 40) non-blank lines
  of its screen and `ready`, rendered concurrently: the oversight table in
  one request (WI-915). The core's `session.sweep` builds on it. Requires
  `sessions`.
- `POST /api/sessions/:id/hibernate` `{"reason"?, "allow_shell"?}` ->
  `SessionSummary`, `POST /api/sessions/:id/wake` `{"env"?, "cols"?,
  "rows"?}` -> `SessionSummary`, and `POST /api/sessions/:id/keep-awake`
  `{"keep_awake": bool}` -> `SessionSummary` — see
  [Hibernation](#hibernation). Each requires `sessions`.
- `POST /api/sessions/:id/role` `{"role": "oversight" | "worker"}` ->
  `SessionSummary` (WI-957). Nominates a session as oversight — one that
  supervises the others — or makes it a worker again. `SessionSpec.role` sets
  it at creation. An oversight session is pinned awake (`keep_awake`) as it
  becomes one, so a restart wakes it by itself; going back to `worker` leaves
  the pin. The role is kept in the session's record, shown on its summary
  (absent for a worker), and the PWA lists oversight sessions first in the
  Places rail. Requires `sessions`.
- `POST /api/sessions/:id/work-item` `{"work_item": "WI-7" | null}` ->
  `SessionSummary` (WI-998). Labels a session with the work item it serves,
  or clears the label with `null` or a blank string. `SessionSpec.work_item`
  sets it at creation. The label is opaque to the engine: trimmed, at most
  200 characters, no control characters (`400` otherwise). It is kept in the
  session's record (a wake and a redeploy keep it) and in its History row,
  and shown on its summary (absent when none). vogt-core sets it from
  `session.start`'s item and re-declares it with the audited
  `session.bind_work`; for a session the core started, the core's
  `coding_sessions.work_item_id` is the truth and the label its copy, and for
  one the GUI started the label is the binding. An agent task run carries its
  task's `vogt_work_item` as the label. The PWA's Places rail shows it as a
  `✦ WI-n` chip. Requires `sessions`. This is an additive field on
  `SessionSpec`/`SessionSummary`, the same shape as `role`; WI-956 slice 2's
  `supervisor` is to be added beside it the same way.
- `POST /api/sessions/:id/conversation` `{"agent", "id", "ended"?}` ->
  `SessionSummary` (WI-962). Links the agent conversation running in the
  session, or with `ended` unlinks it while it is still the session's. See
  [A conversation reported from inside the session](#a-conversation-reported-from-inside-the-session).
  Requires `sessions`. The same body to `POST /api/agent-auth/conversation`,
  authenticated by the session's own broker token instead, is how the agent's
  hook reports.

The 64 KiB input cap mirrors `ws::MAX_INPUT_BYTES`, so the same paste is
accepted or refused whichever transport carries it. Over the cap is `400` on
HTTP; over the WebSocket the frame is dropped silently, because there is no
reply channel to refuse into.

`SessionSummary` carries an optional `command` field — the explicit command
the session was created with; absent for default-shell sessions. It also
carries `activity_changed_at`, the wall-clock instant when the current
activity state began. The same timestamp accompanies `activity` events, so a
client can keep an attention-sorted selection tied to session identity while
live ordering changes; an empty or absent value remains accepted from an older
engine.

`SessionSpec` (the `POST /api/sessions` body) carries an optional `prompt`
field — the brief the session's agent should start from:

```json
{
  "name": "VOGT-42 fix the flaky forge test",
  "cwd": "apps/vogt",
  "prompt": "Fix the flaky forge test.\n\nWhy: it blocks the release."
}
```

`SessionSpec` also carries optional `model` and `effort` — which
model the agent CLI in `command` should run, and how hard it should think. The
engine turns them into that CLI's own flags (`agent_cli.rs`):

| Command | `model` | `effort` |
|---|---|---|
| `claude` | `--model <id>` | `--effort <level>` |
| `codex` | `-m <id>` | `-c model_reasoning_effort=<level>` |
| `opencode` | `--model <provider/id>` | *refused — it has no effort control* |
| `klaudia` | `--model <id>` | *refused — it has no effort control* |

Three rules, each written against a specific failure:

- **A command with no mapping is refused, never started plain.** A session
  that quietly ignored `model` would spawn, run, answer, and be the wrong
  model — a failure with no symptom. The refusal names the binary.
- **The values are validated before they become argv.** They arrive from an
  LLM tool call and are handed to a process spawn, so a model id is letters,
  digits and `. _ - / :` only. A leading dash is refused by name: a "model id"
  that is really `--dangerously-skip-permissions` turns a session into a
  different session, and the card the user approved said *model*.
- **The flags go on the end**, because every supported form ends with the
  agent binary — `vogt-agent-auth run -- claude` included, which is what
  every protected template looks like.

```json
{
  "name": "scratch/scratch",
  "cwd": "scratch",
  "command": ["vogt-agent-auth", "run", "--", "codex"],
  "model": "gpt-5.6",
  "effort": "medium"
}
```

`SessionSpec` also carries an optional `resume` — the agent CLI's own id for a
previous conversation to continue instead of starting a new one:

| Command | `resume` |
|---|---|
| `claude` | `--resume <id>` |
| `codex` | `resume <id>` — a subcommand, so inserted straight after the binary, ahead of the template's own arguments and the model flags |
| `opencode` | `--session <id>` |
| `klaudia` | `--resume <id>` |

It follows the same rules as `model`: refused (`400`) for a command with no
mapping or the default shell, and validated before it becomes argv — letters,
digits and `. _ -` only, at most 128 characters, never a leading dash.

**Which conversation id a session has.** A fresh Claude Code launch whose
command is the bare agent (nothing of the template's own after `claude`, as
in every protected template) is started with `--session-id <engine session
id>`, so its conversation id *is* the engine session id — the `id` every
session listing, `engine_session_id` in vogt-core, and session history
already show. A session lost to a redeploy is resumed with
`resume: <that id>` and the `claude` template. A resumed Claude session keeps
the id it resumed (no `--session-id` is added), and a command that carries
arguments of its own after `claude` is not pinned, because Claude Code refuses
`--session-id` next to a `--continue` or `--session-id` it already has.
Klaudia (WI-950) is pinned the same way: a bare launch gets `--session-id
<engine session id>` and a wake `--resume <id>`; with no engine id to give
it, a fresh launch gets `--new-session`, because Klaudia's TUI otherwise
resumes the newest conversation in its directory. Its brief pointer goes in with
`--prompt-interactive=<text>`, never as a positional prompt: Klaudia reads a
positional prompt as `-p` (answer once, then exit), so the session would end
after its first turn. The pinned Klaudia must have that flag (msp-klaudia
WI-953); a runtime pin to an older commit starts and refuses it. Codex
and OpenCode cannot be told an id at launch. Codex's is not knowable to the
engine; find it with `codex resume` (its picker) inside the pod. OpenCode's
is **captured** after launch (WI-930): the engine reads opencode's own store
(`$XDG_DATA_HOME/opencode/opencode.db`, read-only) for the session whose first
prompt names this engine session's brief, which tells apart several sessions
started in one directory at once. A session started with no brief takes the
newest opencode session in its directory since it spawned that no other
session holds; that is a guess, logged as `basis=directory`. The capture is
logged as `event=launch.conversation`, and the summary's `conversation` then
names it.

##### A conversation reported from inside the session

All of the above is for an agent the engine launched. A `claude` a person
types into a plain shell was, to the engine, a shell: it could not be
hibernated or resumed, a restart dropped its record, and History showed it as
`bash`. That is how an oversight session was lost on 2026-10-07 (WI-962).

So the agent says which conversation it runs. The pod entrypoint installs
`vogt-claude-session-hook` (`engine/deploy/claude-session-hook.sh`) into
Claude Code's user settings (`${CLAUDE_CONFIG_DIR:-~/.claude}/settings.json`)
as a `SessionStart` and a `SessionEnd` hook, beside whatever the file holds;
`ENGINE_AGENT_CONVERSATION_HOOK=0` skips it. Every `claude` in the pod then
runs it. Inside an engine session (`VOGT_ENGINE_SESSION_ID` is set) it posts
`{"agent": "claude", "id": <session_id from the hook's input>}` to
`POST /api/agent-auth/conversation` with the session's broker token, or, when
the deployment brokers nothing, to `POST /api/sessions/:id/conversation`
with `VOGT_HTTP_TOKEN`. `SessionEnd` posts the same with `"ended": true`. The
hook prints nothing, never fails the CLI, and ignores a `claude` whose stdin
is not a terminal: a `claude -p` an agent runs from a tool call is not the
session's conversation.

The engine validates the agent and the id (`agent_cli::is_conversation_id`),
sets the session's `conversation`, writes it into the record, records it on
the History row and logs `event=session.conversation` (audit). From then on
the session is resumable like any agent session: it hibernates, a restart
keeps it (`trigger: "recovered"`, `resumable: true`), and a wake starts the
agent, not the shell, on that conversation, through the template that runs
the agent (the record's own when it does, else the template the agent's name
resolves to, else the bare agent). `/clear` reports the new conversation, and
the end of the old one is then ignored, because an `ended` report unlinks only
the session's current conversation. A session that is hibernating ignores
reports, so an agent's exit as the engine stops it cannot change what the
record resumes. Verified on vogt-dev on 2026-10-07: the hook fires for a
`claude` (2.1.289) typed into an engine shell and sees
`VOGT_ENGINE_SESSION_ID`, the broker URL and token. It does not see
`VOGT_SESSION_ID`, which only a session vogt-core started has.

**Where a resumed conversation starts.** Claude Code and Codex key a
conversation to the directory it ran in, and `claude --resume <id>` finds it
only from there — often not a registered project root (a parent folder, a
worktree). For `claude` and `codex`, the engine looks the id up in the
transcript under `$HOME` (`~/.claude/projects/*/<id>.jsonl`,
`~/.codex/sessions/**/rollout-*-<id>.jsonl`, or Klaudia's
`~/.klaudia/sessions/*/<id>.jsonl`), reads the `cwd` it records, and
starts the session there instead of the requested `cwd` — but only when that
directory exists inside `workspace_root`; otherwise the requested `cwd` is kept
(`engine/server/src/transcripts.rs`). The session summary's `cwd` reports
where it actually started, and vogt-core records that.

The brief's text is not passed to the child. The engine writes it to
`state_dir/agent-task-prompts/sessions/<session-id>.md` before the PTY is
spawned and exports that path as `VOGT_ENGINE_AGENT_TASK_PROMPT_FILE` — the same
variable a scheduled agent task run sets, so an agent started for a work item
and an agent started by a schedule are configured identically. A `prompt` that
is absent, empty, or all whitespace writes no file and sets no variable.

When the command is an agent CLI the engine knows (`claude`, `codex`,
`opencode`, wrapped or not), it is also given a **first prompt** pointing at
that file, so an agent started with a task begins it instead of opening idle:

> Vogt started this session with a brief in `<path>`. Read that file now. If
> it has a "Task" section, carry that task out; otherwise summarise the brief
> in one line and wait for instructions.

It is the positional prompt for `claude` and `codex` (after every flag above)
and `--prompt` for `opencode`. Only this fixed sentence and the path reach
argv — never the brief, which would hit argv limits, need quoting, and stand
in `ps` for every process in the pod to read. vogt-core folds a `session.start`
`task` into the brief as its `## Task` section. A plain shell, or any command
the engine does not recognise, gets no first prompt and keeps only the
variable.

**Quiet defaults for Claude Code.** Engine sessions are often driven by
another agent through `POST /api/sessions/:id/input`, and Claude Code's
interactive niceties become traps there. Every session whose agent binary is
`claude` starts with these variables, placed *before* the template's and the
caller's env so either can override them:

| Variable | Why |
|---|---|
| `CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION=false` | A greyed-out suggested next prompt is accepted by a bare Enter, which a driver sends to submit what it typed. |
| `CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1` | The session-quality survey takes the next keystrokes as its answer. |

The "Teach auto mode about your environment?" dialog has no variable. Its
only off switch is `autoModeEnvSetup.dismissed` in Claude Code's global config
(what "Don't show again" writes), so the pod entrypoint sets that flag in
`${CLAUDE_CONFIG_DIR:-$HOME}/.claude.json` at boot, before any session can
run Claude Code; `ENGINE_AGENT_QUIET_ONBOARDING=0` skips it.

Claude Code also asks two questions once per working directory: whether to
trust the folder, and whether to allow a `CLAUDE.md` that imports files from
outside it. Before each Claude session starts (or wakes), the engine
records both answers for the session's `cwd` in that same config, under
`projects["<cwd>"]`: `hasTrustDialogAccepted`,
`hasClaudeMdExternalIncludesApproved` and
`hasClaudeMdExternalIncludesWarningShown`. The `cwd` is always inside
`workspace_root`, which the operator trusts by deploying it. The write is
atomic and best effort: a config that does not parse is left alone. It
honours a `CLAUDE_CONFIG_DIR` in the session's env, and
`ENGINE_AGENT_QUIET_ONBOARDING=0` skips it as well. A session started with a
brief also gets `--add-dir=<the brief's directory>`. The brief sits under
`state_dir`, outside the working directory, and without that flag reading it
would stop at a permission prompt. The `=` form matters because `--add-dir`
is variadic: the separate form would take the first prompt as a second
directory. A woken session is sent neither the brief nor the flag. Spinner tips are
left on: they are text, not a prompt, and wait for no answer. A dialog
Claude Code adds later can still appear; a driver dismisses one with `Esc`
(sent as `\u001b` over `/input`), or picks an option with the arrow keys and
`Enter` (`\r`).

`cwd` is resolved against `workspace_root` and must stay inside it; a path
that escapes via `..` is `400` rather than a shell in `/etc`. `name` is trimmed
and must be non-empty and at most 256 bytes, on creation and on rename alike;
outside that it is `400`, never truncated. Names need not be unique —
duplicates are confusing, not invalid. `SessionSpec` also carries an optional `template` — a session template *name*
or *tag* the engine expands into `command` (and env) against its configured
`session_templates`, when no explicit `command` is given (the GUI copies a
template's command into the spec itself, so it never uses this). A name matches
case-insensitively; failing that a tag matches, and an `agent`-tagged template
wins an ambiguous tag so `template: "claude"` reaches the deployment's
protected `["vogt-agent-auth", "run", "--", "claude"]` rather than a bare
binary. An unknown name is `400` listing the configured templates, never
silently a shell. This is where vogt-core sends the bare agent name it was
asked for and lets the deployment decide what protected command it runs.

The remaining `SessionSpec` fields —
`command`, `env`, `cols`, `rows`, `scrollback_bytes` — each fall back to the
server's configured default when omitted.

`DELETE /api/sessions/:id` removes the session's prompt file; killing does not,
because a killed session is still inspectable. Prompt files left behind by a
crash or a restart are collected by
`POST /api/agent-tasks/artifacts/cleanup`, which removes every session prompt
whose session the registry no longer holds and reports
`removed_session_prompt_file_count`. `keep_latest_runs_per_task` does not apply
to them: session prompts are retained by liveness, not by count.

`SessionDetail` contains the session summary plus scrollback snapshot metadata:

```json
{
  "summary": {
    "id": "uuid",
    "name": "terminal",
    "activity": "idle|running|waiting-for-input|exited|errored",
    "exit_code": null,
    "alive": true,
    "scrollback_bytes": 123,
    "cwd": "apps/vogt",
    "created_at": "2026-07-06T00:00:00Z",
    "activity_changed_at": "2026-07-06T00:04:12Z"
  },
  "scrollback_pos": 123,
  "scrollback_base64": "..."
}
```

#### Activity states

`SessionSummary.alive` is the fact: `true` while the child process runs,
`false` from the moment it exits (exactly when `exit_code` is set). An exited
session **stays in `GET /api/sessions`** — its scrollback and screen are still
readable — until it is deleted, so a caller that wants running sessions
filters on `alive`, never on presence in the list. An engine that predates
the field omits it; read it then as `exit_code == null`.

`activity` has three terminal states, which are facts, and three live
states, which are a *heuristic*:

```text
live (alive: true)
  spawn                       -> running
  running, quiet for activity_idle_after_ms -> idle
  prompt pattern in the tail  -> waiting-for-input
  new output, no prompt       -> running

terminal (alive: false, never changes again)
  a stop was requested, then any exit -> stopped
  child exits with 0          -> exited
  child exits with non-0      -> errored
```

- `exited` — the child exited with code 0. Terminal.
- `stopped` — someone asked the session to stop
  (`POST /api/sessions/:id/kill`, vogt's `session.stop`) and it then exited,
  whatever its code (WI-913). The stop is recorded before the signal, and
  the summary's `stop` says `by`, `reason` and `at`. History records
  `end_reason: stopped`. Terminal.
- `errored` — the child exited non-zero **without** a requested stop. A
  signal from outside the engine (the OOM killer, a `kill` in a shell)
  counts, because it can be a real failure. Agents that fan out child
  sessions and reap them with `session.stop` leave `stopped` rows, so a crash
  still stands out. Terminal.
  Once either is set the activity never changes again: output the PTY reader
  drains after the exit cannot move it back to a live state (before WI-830 it
  could, which is how a stopped session read `running` forever).
- `waiting-for-input` — the last ~512 visible bytes match a prompt pattern
  (`[y/n]`, a password prompt, a numbered approval menu, a bare `❯`). The
  pattern set is deliberately conservative, because a false positive sends a
  push notification to someone's phone.
- `running` — output arrived within `activity_idle_after_ms`.
- `idle` — no output for longer than that, or none ever. Only ever a live
  process: a session whose program finished reads `exited`/`errored`, not
  `idle`.

The live states are computed in `engine/server/src/activity.rs` from the tail
of ANSI-stripped scrollback, and a client should render them as a hint rather
than treat them as a fact about the child process.

A session that goes quiet without printing a recognizable prompt therefore
reads as `idle`, not `waiting-for-input`. That gap is what the idle-stall
watcher covers: after `idle_stall_after_ms` of continuous `idle` it sends one
notification, and re-arms only once the session leaves the state. It is
switched **off** for a new subscription — a heuristic about silence is not one
of the four kinds worth a phone interruption by default (see Push APIs) — so
the watcher runs and dispatches to whoever asked for it and to nobody else.

#### Reading the screen

`GET /api/sessions/:id/screen` is for a program driving a session — an agent
typing into another agent's TUI. The raw scrollback (and the history log) is
the byte stream the program wrote: redraws, spinner frames and dismissed
menus pile up, and cursor-positioned text loses its spaces once escapes are
stripped. This route instead answers with what the terminal shows now:

```json
{
  "id": "uuid",
  "cols": 120,
  "rows": 40,
  "lines": ["╭────────╮", "│ > fix the flaky test", "╰────────╯", "", "…"],
  "cursor": { "row": 1, "col": 22 },
  "title": "claude",
  "activity": "idle",
  "alive": true,
  "ready": true
}
```

- `lines` — the visible rows, top to bottom, trailing spaces trimmed; always
  exactly `rows` entries.
- `cursor` — zero-based row and column.
- `title` — the last window title the program set (OSC 0/2), or `null`.
- `activity`, `alive` — as in `SessionSummary`.
- `ready` — the program is waiting for input: the session is alive and
  either `activity` is `waiting-for-input`, or it is `idle` and one of the
  lowest ten non-blank lines, box border stripped, starts with a prompt glyph
  (`>`, `❯`, `›`, `>>>`) — Claude Code's input box, Codex's composer, a REPL.
  `running` is never ready, because those TUIs keep drawing their input box
  while they work; nor is `awaiting-approval`, where typing answers a dialog.
- `scrollback` — with `?scrollback_lines=N`, up to N lines that scrolled off
  the top of the screen, oldest first (bounded by the replayed 1 MiB). Absent
  otherwise.
- `turn_started_at`, `last_output_at`, `approval` — as in `SessionSummary`
  (below).

**Turn timing.** `SessionSummary`, the screen and the `activity` event carry
`turn_started_at` — when the session last went `running` from `idle` or
`waiting-for-input` (or was spawned); an answered permission dialog continues
the same turn — and `last_output_at`, when the PTY last printed. A long turn
has a recent `last_output_at` (agent TUIs animate while they work); a hung one
has `running` long gone stale, or an old `last_output_at`.

**Permission dialogs (`awaiting-approval`).** Claude Code and Codex stop and
ask before a command or edit their permission rules do not allow, and Claude
Code denies by itself when a countdown runs out. The engine recognises such a
dialog on the *rendered* screen — a question line ("Do you want to
proceed?", "Do you want to make this edit to …?", "Would you like to run the
following command?", "Allow command?") with a numbered option (`❯ 1. Yes`,
`› 1. Yes, proceed`) under it — and reports `activity: "awaiting-approval"`
with an `approval`:

```json
{
  "question": "Do you want to proceed?",
  "command_excerpt": "Bash command\nsh -c 'docker stop x && docker rm -f x'\nStop and remove the test container",
  "deadline_seconds": 61,
  "deadline_at": "2026-10-05T00:18:00Z",
  "detected_at": "2026-10-05T00:16:30Z"
}
```

`command_excerpt` is read from the screen and the scrollback above it, so a
command taller than the screen arrives whole (up to ~4000 characters).
`deadline_seconds` is computed at read time from the first sighting. The
check runs in the PTY reader only when the raw tail mentions a dialog (a cheap
prefilter), and then renders the screen to confirm it, so a dialog that was
answered and redrawn over is not reported from text lingering in the tail
(`engine/server/src/approval.rs`). Entering the state publishes an `activity`
event carrying `approval`, sends a push (on the `waiting-for-input`
preference, titled "… needs approval", with the deadline and command in the
body), and vogt-core's Inbox shows the session as "asking for approval". To
answer: read the excerpt, then `session.input` the option's number (or arrows
and `enter`), or `esc` to decline.

The engine keeps a terminal emulator per live session. The PTY reader feeds
every chunk into a `vt100` grid (`Terminal` in `engine/server/src/screen.rs`,
`vt100` 0.16.2), and `/screen` reads that grid — visible rows, cursor, title
and up to 2000 lines of scrollback — rather than replaying raw bytes. This
reverses the earlier decision to keep no emulator, and it was forced: a
diff-painting TUI such as opencode (OpenTUI) draws one full frame and then
only the cells that changed, with no newline at all, so once more than the
replay window of diffs had followed the last full repaint, replaying a tail
onto a blank grid showed only the recently changed cells — a spinner and a
progress bar on an otherwise blank screen (WI-990). A grid that has parsed
every byte since the session started still holds the whole frame.

The grid is what makes an unsupported sequence harmless. `vt100` consumes DEC
private modes it does not act on, including 2026 (synchronized output, which
it treats as a no-op so a frame is never held back), drops OSC queries, and
ignores anything else it does not implement, so no control byte can reach a
cell as text. It also tracks the alternate screen and scroll regions, and a
PTY resize reflows the grid to the new size. The grid keeps 2000 scrollback
lines, which bounds what one session's emulator holds; parsing a chunk is the
cost added to the PTY reader's hot path.

A hibernated session has no live grid — it was dropped with the process — so
its screen is still rendered by replaying the tail of output it kept. A live
session's attach sends the grid's frame instead of the raw ring (WI-121); see
[Attach protocol](#attach-protocol).

**Readiness for a driver.** Watch `GET /api/events` for the `activity` event
of your session: `waiting-for-input` is the push signal that a prompt is up,
and `awaiting-approval` that a permission dialog needs an answer.
An agent TUI whose prompt the tail patterns do not recognise reaches `idle`
instead; on `activity` → `idle`, read `/screen` and check `ready`. An
`activity` of `exited`/`errored` (or a `session-killed` event) means the
program is gone and nothing will read input again.

#### Driving a session

The loop is the same whichever surface carries it: **start → wait until
ready → read → answer → stop.** Over MCP (the core's tools, preferred):

1. **Start with the task.** `session_start` with `project` or `work_item`,
   `template: "claude"` (or `codex`, `opencode`), `task` and `reason`. The
   core folds `task` into the brief as its `## Task` section; the engine
   writes the brief to the prompt file and starts the agent on the first
   prompt above, so it begins the task without being typed to. The result
   carries both ids. To continue an earlier conversation instead, pass
   `resume` (see `SessionSpec.resume` above).
2. **Wait until ready.** `session_wait` (`until: "ready"`, `timeout_s` up
   to 600) blocks on the engine until `ready` — or until the session needs a
   person (`outcome` `awaiting-approval` or `blocked`) or exits — and returns
   the screen; one call replaces a polling loop. Over HTTP, the same is
   `GET /api/sessions/:id/wait`; or watch `GET /api/events` for the
   session's `activity` event and read `/screen` on `waiting-for-input` or
   `idle`, as described in [Reading the screen](#reading-the-screen). A
   session whose agent reported itself `blocked` is waiting for a person:
   do what it asks (or answer it), do not re-prompt it.
3. **Read.** `session_last_reply` returns the agent's last replies whole
   and current, from its own Claude Code / Codex transcript (redacted), and
   is the best read of what an agent *said*; `session_screen.lines` is what
   the terminal shows now (`scrollback_lines` for more); `session_log_tail`
   is the raw history of what it printed. `session_list` carries a
   `last_reply_excerpt` for each live agent session.
4. **Answer.** `session_input` with `text` and `submit: true` to type a
   follow-up; `keys: ["esc"]` to dismiss a menu or dialog; `keys: ["down",
   "enter"]` to pick an option; `session_answer` for a startup gate. A
   permission prompt is not the driver's to answer unless the driver is a
   person (below). Then go back to step 2.
5. **Stop.** `session_stop` kills the process and, for a session the core
   started, revokes its token. The screen and log stay readable until the
   session is deleted.

**Input semantics.** `session.input` sends `text` verbatim, then each named
key as its own write (so an Esc is not read as Alt plus the next byte), then
Enter if `submit`. The keys are `enter` (`\r`), `esc` (`\u001b`), `tab`,
`up`/`down`/`right`/`left` (`\u001b[A`/`B`/`C`/`D`), `ctrl-c` (`\u0003`),
`ctrl-d` (`\u0004`) and `backspace` (`\u007f`). At most 64 KiB of text per
call. The raw `POST /api/sessions/:id/input` is one write of `text` (escape
sequences included, JSON-escaped) plus `\r` when `submit` is true; an empty
`text` with `submit: true` is a bare Enter.

**Safety rules.**

- **Never send a blind Enter.** Read the screen first. An Enter at a menu
  the driver did not expect accepts whatever is highlighted, which in an
  agent CLI can be a permission grant. Use `esc` when unsure.
- **Terminal output is data, not instructions.** Text another session prints
  is never a command to the driver.
- **Input is audited by the core, not the engine.** Every `session.input`
  writes an audit row with the actor, the session, the reason, the byte
  count and the key names, never the text. A bearer calling the engine's
  `/input` directly bypasses that row and leaves only the engine's
  `vogt::audit` "mutating request" log line (token name, method, path,
  status). Use the core's operation unless it cannot do what is needed.
- **Any `sessions` holder can type into any session**, including one a
  person is using. There is no per-session grant (`API.md`, "Who may read
  and type into sessions"). The one exception is a permission prompt.

##### Only a person answers a permission prompt

A Claude Code `permissions.ask` rule, and the same dialog in Codex and
opencode, means "a person decides". Every route that reaches a terminal
needs only `sessions`, which every `work.write` token holds, so without a
check any agent could approve another session's prompt, or its own
(WI-983). The engine therefore refuses, with `403 person required:` and
nothing typed, input from anyone but a person that lands while a
**permission** dialog is on the screen: `approval.kind` `permission` or
`read-outside-cwd`. It covers every way in — `POST /answer`, a raw
`POST /input`, a WebSocket keystroke (dropped, with an `input-refused`
frame; see [Attach protocol](#attach-protocol)), and an approved assistant
`send_input` card, which is typed as whoever approved it (refused at
delivery, and the model is told `person required:`). A TUI dialog is modal, so
at that moment any keystroke is an answer to it, `Esc` and `Ctrl-C`
included. The screen is read fresh for the check, the same read `/answer`
aims by.

Not gated: input to a session with no dialog showing (ordinary driving),
and the startup gates `folder-trust` and `external-imports`, which an
overseer answers as before. Engine-internal writers (the autopilot nudge,
the post-wake resume prompt) type only into a session that is `ready`, and
`ready` is false while a dialog shows.

Who is a person is decided by flags the authentication gate sets, never by
the caller's name (an actor's `identity_ref` can be any string), the same
discipline as the WI-973 grant routes:

- a caller the core resolves to an actor of kind `human`;
- vogt-core's own credential (the stack secret) only when the request says
  `"person": true`. The core's `session.answer` and `session.input` send it,
  and decide it from their own authenticated principal the way
  `session.grant_decide` does: never for an agent principal (a session's
  `agent:session:` token, an `agent:engine:` token, the pod token), and never
  for the engine's own credential, whatever actor that is bound to;
- the break-glass `ENGINE_TOKEN`, an operator credential no session holds,
  unless the request says `"person": false` (as the core does when it relays
  an agent with it).

Everyone else — every agent-bound token, whatever its scopes — is refused,
and `"person"` in its request is ignored. Refusals and a person's answers to
permission prompts through `/answer` are logged under `vogt::audit` as
`event=session.permission_answer` with the principal, session, kind,
question and (for an answer) the option chosen. The core's `session.answer`
audit row names the actor, and its `session.answered` event records `kind`,
`question`, `option`, `label` and `person`; `session.input` records
`person`.

The refusal tells the overseer to escalate: leave the prompt for a person in
the Inbox (it is already there as "asking for approval"), or report it with
`session_report_blocked`; stopping the session is still allowed.

This is a boundary only against a session that cannot act as the engine. A
session running as the engine's uid can read the stack secret or the
break-glass token and relay its own `"person": true`; separating the uids is
WI-982. Detection is the same screen reading that raises `awaiting-approval`,
so a dialog the engine does not recognise is not gated, and input that
reaches the terminal in the instant before a dialog is drawn is not gated
either.

Over HTTP, the same loop with `curl`:

```bash
AUTH="Authorization: Bearer $VOGT_HTTP_TOKEN"
ID=$(curl -s -H "$AUTH" -H 'content-type: application/json' \
  -d '{"name":"helper","template":"claude","prompt":"## Task\n\nRun the tests."}' \
  "$VOGT_ENGINE_URL/api/sessions" | jq -r .id)
curl -s -H "$AUTH" "$VOGT_ENGINE_URL/api/sessions/$ID/screen" | jq '.ready, .lines'
curl -s -H "$AUTH" -H 'content-type: application/json' \
  -d '{"text":"\u001b"}' "$VOGT_ENGINE_URL/api/sessions/$ID/input"   # Esc
curl -s -H "$AUTH" -H 'content-type: application/json' \
  -d '{"text":"also run the linter","submit":true}' "$VOGT_ENGINE_URL/api/sessions/$ID/input"
curl -s -H "$AUTH" -X POST "$VOGT_ENGINE_URL/api/sessions/$ID/kill"
```

#### Permission posture

A driven session usually has no person watching it, so its permission
checks decide what it can finish on its own (WI-926; the design is
`docs/design/driven-session-permissions.md`).

- **Default.** No posture flag is passed, so Claude Code runs in its own
  default mode. On a pod that has accepted auto mode, that is **auto**: a
  classifier judges each action against built-in rules, and a block is a
  denial, with no dialog. Every engine-launched Claude session also gets the
  deployment's **driven-session policy** with `--settings=<file>`. This is an
  `autoMode` section whose `environment` and `allow` lists start with
  `"$defaults"`, so they add to Claude Code's rules and replace none. The
  image ships `/usr/local/share/vogt/driven-session-settings.json`:
  - it states that the session is driven, and that a denied action is
    reported, never routed around;
  - it adds exceptions in the classifier's own wording, with no pattern
    rule (`permissions.allow`/`ask`/`deny`), so the classifier judges every
    command:
    - **Read-Only Inspection**: an exhaustive list of commands that only
      report state (`docker ps|logs|stats`, `docker inspect --format` of
      enumerated non-environment fields, `ss`, `netstat`, `git
      status|log|diff|show|branch`, `gh pr view|list|checks|diff`, `gh run
      view`, and a credential-free GET to `127.0.0.1` or `localhost` with no
      body or added headers) is not *Modify Shared Resources* or *Interfere
      With Workloads*. A command it does not name is not covered. Every
      other `docker` subcommand, a whole-object or `.Config.Env` inspect,
      Komodo, Infisical, metadata addresses, non-GET requests, other
      processes' environments, printing a secret and sending data off the
      host stay blocked; *Data Exfiltration* and *Production Reads* are
      never cleared.
    - **Credential Presence Check**: whether a credential this session
      already holds is set or unset, and nothing else: not its length, a
      substring, a hash or a comparison. Only the session's own environment
      and files and the `AGENT_AUTH_*` name lists; never `/proc/*/environ`,
      the engine's or core's environment, `/run/secrets`, another credential
      store or the vault. Fetching stays with the manifest and the grant
      flow.
    - **Own Green PR Merge**: merging a pull request the agent opened for its
      task, in a repository listed under `Autonomous-merge repositories`,
      after its required checks pass, with the plain merge command.
      `--admin` and force options, other people's PRs, unlisted repositories
      and red or pending checks stay blocked. Any other merge is still the
      classifier's *Merge Without Review*: it is not turned into a prompt,
      because any agent with `work.write` can answer a session's prompt
      (WI-983).

  The shipped list is *none configured*, so the image changes nothing until
  a deployment names its repositories. `ENGINE_AGENT_CLAUDE_SETTINGS` points
  at a deployment's own file, which is where estate facts belong: secret
  stores, deploy targets, sensitive hosts. Empty means the image's policy;
  `off` turns the policy off. Prod-mutating, destructive, shared-resource and secret-exposing
  actions keep their built-in rules and stay denied.
- **`permission_mode` on `POST /api/sessions`**: `accept-edits` maps to
  `--permission-mode acceptEdits` (edits accepted, everything else asks).
  `bypass` maps to `--dangerously-skip-permissions` (no checks). Klaudia
  takes `bypass` the same way; its default posture is its own `autonomous`
  mode (finish the task, ask before changing the machine) behind its host
  guardrail, and `accept-edits` is `400` for it: Klaudia's `acceptEdits` is
  only an alias for `autonomous`, so it would not mean what was asked. For
  opencode, see below. Codex is `400`, as is a posture on a plain shell. The posture
  shows on the summary (`permission_mode`, absent for the default), is kept
  in the hibernation record, and is reapplied on wake. vogt-core grants
  `bypass` only to a person; an agent asking for it is refused. An
  agent in a session the engine started itself (the GUI, a protected
  template) counts as an agent too: the engine gives every Claude Code,
  Codex or opencode session it launches without a credential of its own a
  token minted for it by vogt-core (`session.token`, bound to
  `agent:engine:<id>`, revoked when the session ends; each mint is logged
  as `event="session.identity"`), not the pod's brokered token, which is
  bound to a person. A plain shell keeps the pod's token. An agent a person
  starts by hand inside such a shell inherits the shell's token: the person
  started it.
- **opencode** (WI-932) has its own permission model, a `permission` block
  in its config. Unconfigured, it stops at "Access external directory —
  Allow once / Allow always / Reject" with nobody there to answer. The
  engine gives every opencode session it launches a posture through
  `OPENCODE_CONFIG_CONTENT`, which opencode layers over the user's own
  config for that session only, so the shared `~/.config/opencode` is never
  edited:
  - **default**: the deployment's opencode policy. The image ships
    `/usr/local/share/vogt/driven-session-opencode.json`, which allows the
    routine tools (reading, editing, searching, web fetch, other
    directories) and **denies** rather than asks for destructive,
    shared-resource and secret-exposing commands: force pushes, `gh pr
    merge`, Infisical secret reads and writes, Komodo stack writes, `sudo`,
    `rm -rf /`. A denial is reported, so the session never waits for nobody.
    opencode matches command patterns and cannot judge context the way
    Claude Code's classifier does, so this list is coarser.
    `ENGINE_AGENT_OPENCODE_CONFIG` names a deployment's own file (for
    example to allow `gh pr merge` where agents may merge their own PRs).
    Empty means the image's file and `off` means no policy. A file whose
    `permission` values are not `allow`/`ask`/`deny` is ignored with a
    warning, because opencode would refuse it and every opencode session
    would fail to start.
  - **accept-edits**: file work allowed, everything else asks.
  - **bypass**: everything allowed. A person's grant only, as for Claude Code.
- **Denials go to a person.** Auto mode draws no dialog for a denied
  action, so `session.answer` has nothing to answer. The brief tells every
  agent to report a denial with `session_report_blocked` and stop. It then
  shows as `blocked` in the Inbox and on the Oversight board.

#### Hibernation

An idle agent session holds a few hundred MiB in its CLI and its MCP
servers. Its conversation is already on disk in the agent CLI's own
transcript. Hibernating it stops the process tree and keeps the rest, so it
can be started again later by resuming the same conversation under the same
engine id (WI-912; the design is `docs/design/session-hibernation.md`).

- **The record.** Every session started through `POST /api/sessions` has a
  record at `state_dir/sessions/<id>.json` (mode 0600) from the moment it
  spawns. The record holds the name, the template, the command as the
  template expanded it, the directory, the model and effort, the agent
  conversation, the brief file and `keep_awake`. It also holds the
  template's and caller's environment *with every secret removed*: anything
  `is_secret_env` matches, including `VOGT_HTTP_TOKEN` and the broker
  token. A session that ends (it exits, is killed or is deleted) forgets its
  record. Agent-task runs have none.
- **What can hibernate.** Only a session whose agent conversation id the
  engine knows. That is a bare Claude Code launch, whose conversation is
  pinned to the session id (`--session-id`), or any agent started with
  `resume`. The summary's `conversation` names it. A fresh OpenCode session
  can hibernate once its id is captured (a few seconds after launch; see
  "Which conversation id a session has"). A fresh Codex session mints its own id, and the
  engine does not guess it. A shell
  hibernates only with `allow_shell`, and wakes as a fresh process in the
  same directory (`hibernation.resumable: false`). An agent-task run is
  never hibernated. A refusal is `409` with the reason.
- **Hibernate.** The engine writes the last 256 KiB of output and the
  terminal size beside the record. It then sends `SIGTERM` to the child's
  process group and to every descendant found before the signal, waits up
  to 5 s, and `SIGKILL`s whatever is left. The plain kill signals only the
  child, which can leave MCP servers running. The session stays in
  `GET /api/sessions` with `activity: "hibernated"`, `alive: false` and a
  `hibernation` object (`at`, `trigger`, `reason`, `resumable`). Its history
  row ends with `end_reason: "hibernated"`, and its broker grant is revoked.
- **While hibernated.** `GET /api/sessions/:id` and `/screen` serve the kept
  output, and reading them does not wake the session; `ready` is false.
  Attach replays the same output and then sends `{"type":"hibernated"}` (see
  [Attach protocol](#attach-protocol)). Input, wait, resize and `blocked`
  answer `409` with a hint to wake the session. `kill` and `DELETE` forget
  it, along with its record, kept screen and brief.
- **Wake.** The session is respawned under the same id, with the recorded
  command, directory, model and environment, plus `resume` of its
  conversation. No brief prompt is sent, because the conversation has
  already read its brief, but `VOGT_ENGINE_AGENT_TASK_PROMPT_FILE` still
  points at the brief file. `env` from the request is set on top. The
  record holds no secrets, so this is how a caller hands a woken session its
  credentials: vogt-core's `session.wake` mints a new `VOGT_HTTP_TOKEN`. The
  engine mints a new broker grant. The history row and log continue.
  Waking a live session returns it unchanged.
- **Restarts.** On `SIGTERM` the engine hibernates every session it can
  (`trigger: "shutdown"`) before the history drain, and an oversight session
  even when it is a plain shell. At boot, every record without a process
  becomes a hibernated session. A record the engine never got to hibernate
  (a `SIGKILL`, a crash) comes back as `trigger: "recovered"`, with its
  screen taken from the history log. A redeploy therefore leaves the agent
  sessions listed and wakeable, not gone. A recovered record with no
  conversation to resume is forgotten, unless it is an oversight session,
  which comes back hibernated with `resumable: false`, so the overseer stays
  listed (WI-962). A shell whose agent reported its conversation (see
  [A conversation reported from inside the session](#a-conversation-reported-from-inside-the-session))
  is resumable and comes back like any agent session.
- **`keep_awake`** pins a session. The pin is kept in its record and shown
  on its summary. A pinned session is never hibernated by policy, and if the
  engine finds it hibernated at boot (after a shutdown or a crash), it wakes
  it by itself. With a core, the engine does this through vogt-core's
  `session.wake`, using the stack secret, so a linked session gets a fresh
  token. The core may still be starting, so the engine retries for a few
  minutes. Pin a driver or oversight session, and it comes back after a
  redeploy by itself. Nominating a session as oversight (`role`, WI-957)
  pins it for you. Only a session with a conversation to resume is woken: an
  oversight shell kept without one stays hibernated, because waking it would
  open an empty shell. An oversight session woken at boot is told so once it
  is at its prompt (WI-962), with one line typed into it:
  `[vogt] This oversight session was resumed after the engine restarted at
  <time>. Check on the sessions you oversee (session_sweep), wake the ones you
  need, and carry on where you left off.` Workers are not told; they wake on
  demand. Nothing is typed into an overseer that is not ready within 5
  minutes or that needs a person.
- **Policy** (`hibernate_policy.rs`) is off unless configured.
  `ENGINE_HIBERNATE_IDLE_AFTER` (`2h`, `30m`, or seconds) hibernates an agent
  session that has had no input or output for that long.
  `ENGINE_HIBERNATE_MEMAVAILABLE_BELOW` (`2GiB`, `512M`, or bytes)
  hibernates the quietest eligible session while the pod's available memory
  is below it, one session per minute. Available memory is the cgroup
  `memory.max` minus `memory.current` when the pod has a limit, otherwise
  (and never more than) the host's `MemAvailable`. Both settings keep every
  exemption, and the watcher logs each one:
  - the session cannot be hibernated at all (see above);
  - it is pinned awake;
  - a turn is `running`, or it is `awaiting-approval`;
  - it is `blocked` on a person;
  - no agent CLI runs in it at all: a shell whose typed-in agent reported its
    conversation and then died without unlinking it, which a wake would turn
    into that agent (WI-962);
  - a shell process (`bash`, `sh`, …) runs *below* the agent CLI, meaning a
    tool call or a background job is at work. The agent's MCP servers are not
    shells, and a wrapper shell that launched the CLI sits above it, so
    neither counts;
  - for the idle trigger only, it is on **autopilot** (below): the pause at
    the end of each of its turns is exactly the quiet the idle trigger looks
    for. Memory pressure can still take it.

  A hook that keeps a turn going reads as `running` and is exempt. A value
  that does not parse stops the engine at startup.

#### Autopilot

A session started with `autopilot: true` (`SessionSpec`; vogt-core sets it
from `session.start`, on by default for an agent template given a task) is
meant to work through a backlog unattended (WI-949). Its brief tells the
agent to carry on to the next item and, when nothing unblocked is left, to
end its reply with a line reading exactly `AUTOPILOT: DONE`. Agents still
stop: opencode ends a *run* at every natural stop of the model, and any
agent may stop to announce its next step. So the engine re-drives it
(`autopilot.rs`):

- a Claude Code, Codex or opencode session on autopilot that is at its
  prompt (`ready`, below), not blocked on a person, and quiet for
  `ENGINE_AUTOPILOT_NUDGE_AFTER` (default `60s`) is sent a one-line "carry
  on" and Enter, logged as `event=autopilot.nudge`, and counted on the
  summary as `autopilot_nudges`;
- a line reading `AUTOPILOT: DONE` among the last lines of its screen turns
  its autopilot off for good (`event=autopilot.done`), as does reaching
  `ENGINE_AUTOPILOT_MAX_NUDGES` (default 100; `event=autopilot.capped`).
  `0` never nudges, leaving autopilot as the idle exemption alone;
- Klaudia is never nudged: it runs its own goal loop.

The summary's `autopilot` reads `false` once it is off. A nudge is input, so
it restarts the quiet clock and nudges cannot stack up.

**When opencode is `ready`.** opencode draws no prompt glyph, so `ready`
recognises its composer separately: the `┃` bar closed by a `╹▀` footer near
the bottom of the screen, with no `esc interrupt` (which it shows only while a
turn runs). Without this, an opencode session was never `ready`, and a driver
waiting for it to be (`GET /wait?until=ready`) only ever timed out.

#### Resource use

Every 10 s the engine reads `/proc/*/stat` once and sums, for each live
session, the subtree below its PTY child. The figures are resident memory,
CPU used since the previous sample (in percent of one core, so a busy tree
can read above 100), and process count. The result rides on the summary as
`resources` and goes out as one `session-resources` event per round. This is
visibility only, not a limit (WI-916; enforcement is WI-895). It lets you see
which session holds the memory from Vogt itself, before the host's OOM killer
chooses. RSS counts shared pages once per process, so a tree that shares a
lot reads high. A process that double-forks away to init is no longer
counted, the same as in `ps --forest`. `ENGINE_SESSION_RSS_WARN` (`8GiB`)
sets `resources.over_threshold` on any session at or over it. The core's
`session.list` takes `order: "rss"` to put the heaviest first.

#### Launch timing, logs and metrics

A session's launch explains itself in the engine log and on `/metrics`
(WI-927). Every line carries an `event=` field to filter on in Loki, for
example `{container="vogt-prod", job="docker/engine"} |= "event=\"launch."` (string
fields are logged quoted: `event="launch.report"`).

| `event=` | Level | When | Fields |
|---|---|---|---|
| `session.start` | info, or warn on failure (audit) | every session the engine starts | `session_id`, `name`, `template`, `origin` (`api`, `agent-task`, `wake`), `launcher`, `spawn_ms`, `outcome`, `error` |
| `launch.first_output` | info, or warn at 10 s or more | the session's first byte of output | `session_id`, `name`, `launcher`, `first_output_ms` |
| `launch.stage` | info | each stage the launch wrapper reports | `session_id`, `stage` (`login`, `secrets`, `bootstrap`), `ms`, `outcome`, and for `secrets` the `project` and `mode` |
| `launch.secrets` | info (audit) | each secret project the launch read | `session_id`, `project`, `mode`, `ms`, `count`, `missing`, `secrets` (`VAR=SECRET_NAME` pairs, never a value) |
| `launch.report` | info, or warn when failed or 10 s or more | the wrapper hands over, or fails | `session_id`, `name`, `command`, `outcome`, `total_ms`, `error` |

`launcher` is `agent-auth` when the launch wrapper runs first, and `direct`
otherwise. The wrapper (`vogt-agent-auth run`/`shell`) times its own stages
and sends one report at handover, or at its failure, to `POST
/api/agent-auth/launch-report`. That route sits beside the secret broker, and
the session authenticates to it with its broker token. It is accepted once per
session; a second report gets 409. A launch that took 5 s or more also says so
in the session itself.

The wrapper reads each secret project with **one** request. It does not run
the vendor CLI once per secret: each CLI run spent about 650 ms on the
vendor's telemetry after the API had answered. Fifteen such runs made a
launch take 10 s on a good day and 85–145 s when that egress was slow. A
project the bulk request cannot read falls back to the per-secret CLI, and
the report says so (`mode=cli`).

`ENGINE_METRICS_ADDR` (`0.0.0.0:9464`) serves `GET /metrics`, in the
Prometheus text format, on a listener of its own. It is never on the API port,
which is the front door. Unset, nothing listens.

| Metric | Type | Labels |
|---|---|---|
| `vogt_session_first_output_seconds` | histogram | `launcher` |
| `vogt_session_launch_seconds` | histogram | `command`, `outcome` |
| `vogt_session_launch_stage_seconds` | histogram | `stage` |
| `vogt_session_starts_total` | counter | `origin`, `outcome` |
| `vogt_session_launch_secret_reads_total` | counter | `mode`, `outcome` |

Every label set is fixed by the code, never a session id or name. For a
session the launch wrapper starts, "usable" is its handover, so the p95 to
watch is
`histogram_quantile(0.95, sum by (le) (rate(vogt_session_launch_seconds_bucket[30m])))`.
First output can come earlier than that (the wrapper prints as it goes), so
`vogt_session_first_output_seconds` is the measure for `direct` sessions.

The engine logs without colour when its stdout is not a terminal, so these
`key=value` fields reach the log pipeline intact.

### Attach protocol

`GET /api/sessions/:id/attach` — WebSocket upgrade. It sits outside the bearer
middleware and does its own auth, because a browser cannot set an
`Authorization` header on a WebSocket handshake. The credential arrives in the
first text frame instead of in the query string, so it does not land in proxy
and access logs. `?token=` is refused unless `ENGINE_WS_QUERY_TOKEN=true`: it
exists only to keep a client that has not been redeployed working, every use
is logged, and it *does* land in those logs.

The first frame's token goes through the same resolver as the HTTP gate and
must carry the `sessions` capability — for a core-resolved caller, a login or
token with `work.write`, `project.write` or `admin`. Attaching is a write:
the socket's binary frames go to PTY stdin. A `read`-only credential can list
sessions (`GET /api/sessions`) and nothing more here: the scrollback
(`GET /api/sessions/:id`), the screen and the attach all need `sessions`.
Through the core it can still read screens and logs (`session.screen`,
`session.log_tail`), because the core calls the engine with its own
credential.

The attach sequence is ordered:

1. client sends auth control frame, optionally with its last applied cursor
   and always with its replay budget:
   `{"type":"auth","token":"...","resume_from":123,"snapshot_tail_bytes":1048576}`
2. server sends `snapshot-start`
3. server sends zero or more binary scrollback chunks
4. server sends `snapshot-done`
5. live PTY traffic continues

Client text control frames:

```json
{"type":"resize","cols":120,"rows":40}
{"type":"ping","id":1}
```

Server text control frames:

```json
{"type":"snapshot-start","session_id":"uuid","scrollback_bytes":0,"scrollback_pos":0,"reset":true,"cols":120,"rows":40}
{"type":"snapshot-done"}
{"type":"resize","cols":120,"rows":40}
{"type":"pong","id":1,"pos":123}
{"type":"lag","note":"client too slow; reattach"}
{"type":"hibernated"}
{"type":"input-refused","reason":"person required: …"}
```

One PTY has one size, however many clients attach. `snapshot-start` carries
the size its payload is drawn at, and every attached socket gets a `resize`
frame whenever any client's `resize` changes the PTY's size (the one that
asked included; a resize to the current size sends nothing). Pending output is
flushed before the frame, so output after it is painted for the new size. A
client draws the stream at that size, not the size its own pane fits: a
diff-painting TUI (Bubble Tea, Ink) repaints with relative cursor moves, and a
narrower client wraps each line so the moves land on the wrong rows and leave
ghost frames (WI-1089). The PWA asks for its own size only from the pane the
person is using — on open, on input or focus, on a resize while focused, or
from its "Fit to this screen" chip — and follows otherwise.

`input-refused` says that input this socket sent was dropped rather than
typed: a permission prompt was showing and the attached caller is not a
person (WI-983, [Only a person answers a permission
prompt](#only-a-person-answers-a-permission-prompt)). The socket stays open;
input once the prompt is gone is typed as usual. A person's keystrokes are
never checked.

`hibernated` follows `snapshot-done` when the session is hibernated. The
snapshot was its kept output (always `reset`, whatever `resume_from` said).
Nothing live follows, and the server closes with code 1000. Attaching never
wakes a session, because the PWA pre-warms panes. Wake it with
`POST /api/sessions/:id/wake` and attach again cold: the woken process's
output starts at position 0.

A `pong` echoes the `ping.id` and carries `pos`: the absolute byte offset the
server has **actually streamed to that socket**, not `total_written`. The pong
is formed and sent by the same outbound task that streams output, after
flushing anything already queued, so it is ordered on the wire after those
chunks and its `pos` can never exceed what the client has received. A client
uses it purely as a liveness probe — a `pos` ahead of what it has rendered is a
suspect to confirm with a second probe, not an immediate reconnect.

`snapshot_tail_bytes` is the replay **budget**, and it bounds every reply — a
client's terminal keeps only a fixed scrollback, so the server never ships more
than the client can hold:

- **Cold attach** (no `resume_from`), and a **warm reattach whose cursor aged
  out or whose delta exceeds the budget:** the live grid's frame — escape
  codes for the current screen, the scrollback the grid holds and the cursor
  (`Terminal::frame` in `screen.rs`) — `reset` true, cut to the budget on a
  ground-state boundary when the frame is larger. This is a few kilobytes
  where the raw ring was up to the whole budget, and it carries the frame a
  tail of raw bytes loses (WI-990, WI-121). `reset: true` may therefore follow
  a `resume_from`. The client discards its stale cursor and re-anchors to
  `scrollback_pos - scrollback_bytes`, so after replaying the frame its
  position is `scrollback_pos` again and live traffic resumes with no gap.
- **Warm reattach whose cursor is retained and whose delta fits the budget:**
  `reset` false and the binary snapshot contains only the newer bytes,
  byte-for-byte — an ordinary switch-away/switch-back appends without a clear.
- **No live grid** (a session restored without one, and every hibernated
  attach): the ground-state-aligned tail of the raw ring, at most the budget,
  `reset` true. A hibernated attach ignores `resume_from` and `snapshot_tail_bytes`
  and replays the output it kept.

The returned `scrollback_pos` is always the absolute end position, unaffected by
trimming the front. Omitting `snapshot_tail_bytes` (the in-band lag resync,
which carries its own cursor) leaves a retained delta unbounded and byte-exact.

A hidden pane does not attach again to catch up (WI-128). While it is hidden it
keeps the socket it already holds open and buffers the frames, writing nothing
to the terminal; on return it writes that buffer when it is within the budget,
or resets and writes a ground-state tail when it is not. Only a pane past the
per-document cap (four) closes its socket, and it comes back through the bounded
attach above. A pane that is unfocused but still visible keeps rendering, so
this never applies to the other half of a split.

A text frame that does not parse as a control message is treated as raw input,
because some tools send keystrokes as text. Snapshot chunks are capped at
64 KiB each. Close codes a client should recognize: `4408` no auth frame
within five seconds, `4401` bad or missing auth frame, `4404` no such session.

### Events and status

- `GET /api/events` -> `text/event-stream` of `ServerEvent`, one JSON object
  per `data:` line. Variants are `session-created`, `session-renamed`,
  `session-killed`, `session-hibernated` (`{id, trigger}`), `session-woken`
  (`{id}`), `session-resources` (`{samples: [{id, resources}]}`, every
  10 s while sessions run; see [Resource use](#resource-use)), `activity` (`{id, state, activity_changed_at}`; `state`
  is one of the activity states above, `exited`/`errored` once the child has
  exited),
  `vogt.changed`, and the agent-task steering
  trio — `task.gate.opened` (`{task_id, run_id, session_id, gate_id, question,
  options}`), `task.gate.answered` (`{…, gate_id, option?, outcome, actor,
  reason?}` where `outcome` is `approved` or `blocked`), and `task.steered`
  (`{…, actor, interrupt, reason?}`) — plus `task.run.concluded` (`{…, outcome,
  exit_code?, duration_ms, retries, branch?, final_sha?, files_changed?,
  insertions?, deletions?, cost_usd?}`), each tagged by `type`. The
  stream carries a `:ka` keep-alive comment every 15 seconds while nothing
  else is happening. A client that falls more than the bus's buffer behind
  receives `{"type":"lagged","skipped":N}` in band and the stream goes on: it
  missed N events, so whatever it built from the stream should be read
  again (the PWA's session store refetches). Every internal subscriber
  carries on the same way, so the push watcher, `session_wait` and the
  agent-task watchers lose the skipped events, never the subscription. Lags
  are counted per subscriber in `/api/status` → `event_lag` (WI-920).
  A client should treat the accepted response and every
  frame, comment included, as proof the front door is alive — a quiet
  session emits no event for as long as it runs — and presume a stream
  dead after about three missed keep-alives, reconnecting without backoff:
  Android in particular lets a backgrounded socket die without ever raising
  an error. The PWA's session store does exactly this.
- `GET /api/status` -> `OperationalStatus` — version, session count, push
  subscription count, live GUI process count, whether the GUI stream and FCM
  are configured, and nested `history`, `agent_tasks`, `auth_broker` and
  `storage` blocks. Storage numbers are counts and byte totals, never paths
  into the workspace beyond the two roots themselves. MCP: `engine_status`
  (`engine.status`), through the core's engine credential.
- `GET /api/agent-clis[?upstream=true]` -> `AgentCliReport` — the
  runtime-pinned agent CLIs and the Go toolchain ([`DEPLOYMENT.md`](DEPLOYMENT.md)
  §3): for each tool in the image's table its package (an npm package, or
  for `kind: "go-dist"` the Go release mirror), `kind`, binary, the variable
  that pins it at boot, the baked version, the active version and its
  `source` (`image`, `runtime` or `absent`), and the versions already on the
  volume. With `upstream=true` the engine also asks npm for each package's
  `latest`, or the Go mirror for its newest stable release (cached an hour;
  omitted when upstream does not answer) and says whether `update_available`.
  Any valid token.
- `POST /api/agent-clis/{tool}` with `{"version": "2.1.261"}` -> the same
  report after the move. Runs `vogt-agent-cli-install` for the tool: an exact
  version, `image` for the baked copy, or a dist-tag the deployment opted
  into. New sessions get the new version; running sessions keep the files
  they started with. `400` for a malformed or refused version, `404` for a
  tool the image's table does not name, `409` when the install or its smoke
  check failed (the previous version stays current; the installer's words are
  in the body). Needs the `agent-clis-write` capability: it downloads and
  executes a package from npm (or a Go release) inside the pod. The pin an environment variable
  sets is re-applied at the next container start, so a move meant to survive
  a restart belongs in the deployment's `.env` as well.

The stream sends a `ka` comment every 15 seconds. A client must not treat that
interval as a timeout budget of its own, but its absence is the fastest signal
that the connection is dead. A client too slow to keep up is dropped from the
broadcast rather than buffered, and the events it missed are simply gone —
`GET /api/sessions` is the resynchronisation path, not a replay of the stream.

The stream is authenticated like every other `/api/*` route, which means the
browser cannot use `EventSource` (it cannot set headers); the PWA reads it
with `fetch` and a `ReadableStream`.

### Assistant APIs

All routes 404 unless the server has `ENGINE_ASSISTANT_API_KEY`
provisioned. Mutating routes require the `assistant` token capability. See
§6 for the threat model and behavior.

- `POST /api/assistant/message` `{"text": "..."}` ->
  `{"reply": string|null, "pending_action"?: PendingAction, "tool_trace"?: string[],
  "created_at"?: string, "session_refs"?: AssistantSessionRef[],
  "actions"?: AssistantTranscriptAction[]}`
- `POST /api/assistant/actions/:id` `{"approve": bool}` -> same reply shape
- `PATCH /api/assistant/actions/:id` `{"reason": string}` -> the updated
  pending action only; Vogt writes accept this preview step, terminal input
  refuses it, and no effector runs until the unchanged POST approval route.
- `GET /api/assistant/history` -> `{"transcript": [...], "pending_action"?: ...}` —
  the **ephemeral** in-memory transcript of the current conversation, ungated.
- `GET /api/assistant/log?limit=&offset=&actor=` -> `LoggedEntry[]` — the
  **durable** interaction log, newest first. Unlike `history` this is a
  cross-conversation record attributable to each actor and surviving restart, so
  it is scope-gated on the `assistant` capability. Each entry is
  `{seq, at, actor, kind, direction, payload}`, where `kind` is one of
  `utterance` / `request` / `reply` / `tool_call` / `tool_result` /
  `pending_action` / `backend_error`. External content in a payload keeps its
  delimiters (see the threat model in §6). `POST /api/assistant/message`
  accepts an optional `utterance` (the raw recognised text before the repair
  pass) so a repaired turn logs both forms.
- `POST /api/assistant/reset` -> `OkResponse`
- `POST /api/assistant/stt` (multipart audio, field `file`, optional text
  field `prompt`) -> `{"text": string}` — server-side transcription. Proxies to
  `/audio/transcriptions` on an ordered, independently-configured base-URL list
  (voicemode semantics: local first, cloud fallback). A `prompt` field is
  forwarded to the backend as its bias prompt (trimmed and bounded to 2 KiB) —
  the mobile client sends the deployment's project and session names there, so
  a transcriber that has never heard `komodo` is told to expect it; a backend
  that ignores the field is no worse off. **404** when unconfigured or every
  entry fails, so the client falls back. Scope-gated on `assistant` (a POST
  under `/api/assistant`). Audio and prompt are proxied, never stored.
- `POST /api/assistant/tts` `{"text": "..."}` -> an audio stream (`audio/*`) —
  server-side synthesis. Proxies `{model, input, voice}` to `/audio/speech` on
  the same kind of ordered list. **404** when unconfigured/all-failed. Audio is
  streamed back and never stored.
- `GET /api/assistant/call` — WebSocket, the live call (see *Live call
  contract* below). **404** unless the assistant, STT and TTS are all
  configured and `ENGINE_ASSISTANT_CALL_ENABLED` is not off, which is exactly
  when `/api/config` reports `assistant_call_enabled: true`.

`PendingAction` is tagged by `kind`, because the assistant has two effectors
and a client must not render one as the other:

- `{"kind": "send_input", "id": uuid, "session_id": uuid, "session_name":
  string, "text": string, "submit": bool}` — the exact bytes the assistant
  wants to type into a session.
- `{"kind": "vogt_write", "id": uuid, "operation": string, "target": string,
  "reason": string, "payload": string}` — a mutating Vogt operation (e.g.
  `work.transition`), the arguments pretty-printed in `payload`, and the
  `reason` Vogt will store in its audit log, surfaced on its own because it is
  the part of the approval that outlives it.

Both await approval at `POST /api/assistant/actions/:id`, one at a time. The
identity on *that* request is the credential a Vogt write is made with: the
approving user's own bearer, forwarded to the core, never a shared one. `GET /api/config`
advertises `assistant_enabled` and `assistant_model` (presence only, never the
key).

A transcript entry is
`{"role", "text", "tool_trace"?, "created_at"?, "session_refs"?, "actions"?}`.
New entries receive a server receipt timestamp. `session_refs` entries are
`{"id", "name", "activity"}` and an action is currently
`{"kind":"open-session", "session_id", "label"}`. The server creates these
only from successful structured session-tool results; they are not inferred
from assistant prose. Persisted entries and older clients remain compatible:
all three display-metadata fields may be absent and then mean no timestamp,
references, or actions. `reply` is null when the turn paused on a pending
action before the model produced any text, which is the state a client should
render as "waiting for you", not as an empty answer.

### Voice turn contract

A spoken turn crosses the same `/api/assistant` routes a typed one does, plus
the two speech seams. The whole contract, and where each half degrades:

1. **Capture -> transcription.** The client records a push-to-talk take and
   `POST`s the audio to `/api/assistant/stt` (or transcribes on-device inside
   the APK). A **404** here means STT is unconfigured: the client retires the
   microphone and falls back to typed input behind a visible notice, never an
   error surfaced as a failure.
2. **Repair.** The recognizer's best guess is run through the client's domain
   repair pass (`WI-7`, project slugs) before it is sent. The repair is shown,
   not applied silently, because a wrong repair is confidently wrong and is what
   gets sent.
3. **The turn.** `POST /api/assistant/message` carries **both** forms:
   `{"text": repairedText, "utterance": rawRecognizedText}`. A typed turn omits
   `utterance` entirely (`{"text": ...}`); only a voice turn sends it, so the
   durable log at `/api/assistant/log` retains raw *and* repaired provenance
   (the `utterance` and `request` entries). `profile` is added only when a
   non-default provider is chosen.
4. **Approval gate.** A reply may carry a `pending_action`; no effector runs
   until an explicit on-screen `POST /api/assistant/actions/:id`
   `{"approve": bool}`. This is identical to a typed turn — a voice turn does
   not relax the gate, and there is no setting that lets the assistant act
   without asking (see the threat model in §6).
5. **Reply -> speech.** A spoken client voices the reply through on-device
   synthesis, or `POST /api/assistant/tts` when it has none. A **404** (or any
   synthesis failure) degrades this half behind a visible notice; the text
   reply is still shown and is *not* treated as a failed turn.

**Cancellation.** Aborting the in-flight `POST /api/assistant/message` (the Stop
control) drops the connection, which the engine treats as a cancellation, and
the composer keeps what was said. Leaving the surface mid-take abandons the
captured audio rather than sending a half-spoken turn, and a queued or playing
TTS clip is stopped the moment the speaker sends again or leaves.

The web client's `assistant*Speech` unit tests exercise these seams headless,
and `web/tests/browser/gui.spec.ts` drives the whole
capture -> STT -> repair -> `{text, utterance}` -> approval -> TTS journey (and
its STT/TTS-unavailable fallbacks) in a real browser with the microphone and
speech routes stubbed.

### Live call contract

`GET /api/assistant/call` upgrades to a WebSocket carrying one spoken
conversation (WI-960). The pipeline is the generic `voxcall` crate
(`engine/voxcall`, its `DESIGN.md` has the trait boundary); the DTOs are
`CallClientEvent` / `CallServerEvent` in `voxcall::protocol`, and event names
follow the OpenAI Realtime API's where the meaning is the same. Vogt's side —
the route, authentication, the providers — is `engine/server/src/call.rs`.

**Opening.** The first frame must be `{"type":"auth","token":"..."}` within
5 s, and the bearer needs the `assistant` capability (`4401` unauthorized,
`4403` lacking the capability, `4408` too slow). One call at a time per
engine — the assistant has one conversation — so a second gets an `error`
event and close `4409`. The server answers `session.created`
`{call_id, sample_rate: 16000, end_of_turn_ms, barge_in_ms}` and
`call.state {state: "listening"}`.

**Client → server.**

- Binary frames: the microphone as little-endian PCM16, mono, 16 kHz (the PWA
  sends 20 ms frames; at most 64 KiB per frame).
- `session.update {profile?}` — the assistant profile the call's turns run on.
- `output_audio.started {response_id, index}` — the client began playing piece
  `index`. What was heard of a cut reply is reckoned from these; a client that
  never sends them is reckoned from the clips' lengths instead.
- `output_audio.idle {response_id}` — the playback queue ran dry.
- `response.cancel` — stop the reply now (a tap rather than a barge-in).
- `action.resolve {id, approve}` — the approval card's buttons. The only way a
  call approves or denies anything.
- `ping` → `pong`.

**Server → client.**

- `call.state {state}` — `listening`, `user_speaking`, `thinking`,
  `speaking`, `awaiting_approval`.
- `input_audio_buffer.speech_started` / `.speech_stopped` — the user's turn
  began / ended (endpointing below).
- `conversation.item.input_audio_transcription.partial {text}` — a live
  caption: the words of the turn so far, while it is still being spoken (the
  whole text each time, not an increment). `.completed {text}` — the turn's
  transcript.
- `response.created {response_id}`; `response.text.delta {response_id,
  delta}` as the model writes.
- `response.audio.start {response_id, index, text, content_type, bytes}`,
  then exactly one **binary** frame: that piece's audio, whole, in the
  container the TTS backend produced. Play pieces in `index` order.
- `response.done {response_id, status, text?, metrics}` — `status` is
  `completed`, `interrupted`, `pending_approval` or `failed`; `text` is the
  reply as the conversation records it (for a cut reply, what was heard).
- `output_audio.clear {response_id}` — stop playing and drop what is queued.
- `conversation.item.truncated {response_id, text}` — a reply that had
  finished generating was cut while being spoken; the conversation now keeps
  only `text`.
- `assistant.pending_action {action}` — an approval card (the same
  `PendingAction` shape as `/api/assistant/history`);
  `assistant.action_resolved {id, approved}` once its button was pressed.
- `error {message}`.

**Turn-taking.** The engine runs voice activity detection on the incoming
audio — `earshot`'s small neural detector by default, or the adaptive
noise-floor energy detector with `ENGINE_ASSISTANT_CALL_VAD=energy`. A turn starts after 100 ms of voice and ends after
`end_of_turn_ms` of silence (default 700); a sound with less than 250 ms of
voice is discarded. STT and TTS are the deployment's own backends, called
exactly as `/api/assistant/stt` and `/tts` call them.

**Streaming transcription.** The turn is transcribed while it is being
spoken (`ENGINE_ASSISTANT_CALL_STT_MODE=chunked`, the default). Whisper
servers decode whole clips only; speaches' `stream=true` yields segments
after a full decode, and its `/v1/realtime` transcribes the buffer on
commit. So the engine streams the turn to the backend as chunks:

- each 200 ms pause cuts what was said since the last cut (at least 1 s of
  it) into a chunk, and speech that runs 6 s without one is cut at its
  quietest 20 ms;
- chunks are transcribed one at a time, in order, each with the turn's
  words so far as the request's `prompt`, so a sentence cut at a pause still
  reads as one, and a single-worker backend never has more than one clip of
  the call queued;
- each finished chunk is sent as a `.partial` caption;
- when the turn ends, only the audio after the last cut is left (usually
  none, because the pause that ended the turn already cut it), trimmed to
  200 ms of its trailing silence — whisper models given the full 700 ms tend
  to fill it with repeated words.

The transcript is the chunks' words joined. If a chunk fails, the turn is
transcribed once more as one whole clip. `ENGINE_ASSISTANT_CALL_STT_MODE=whole`
is that whole-clip path throughout: the turn so far is transcribed at each
pause and discarded if the user talks on, otherwise the whole turn when it
ends. Use it for a backend that transcribes short clips badly.

**The reply.** The transcript runs as an ordinary assistant turn, *streamed*
(see *Streamed turns* in §6) and told it is on a call (short plain
sentences). The reply is cut into sentences as it arrives — the first at its
first clause, to start sooner — and each is synthesized and sent the moment
it exists, while the model is still writing the rest. When the model starts
a tool round before saying anything, a filler line (`ENGINE_ASSISTANT_CALL_FILLER`,
default "One moment.") covers the wait.

**Barge-in.** While a reply is generating or playing, `barge_in_ms` of voice
(default 500) stops it: the turn is cancelled, `output_audio.clear` is sent,
and the reply is cut back to what had started playing, flagged `interrupted`
in the transcript. A shorter sound over a reply (a "mm", or echo) is ignored.
While the reply plays, the detector demands more of a frame, since what
the client's echo cancellation leaves of the reply is the likeliest false
trigger.

**Approvals.** A turn that proposes a change ends at the gate exactly as a
typed one: the card is sent as `assistant.pending_action` and a short line
says it is on screen. **Nothing spoken approves it.** While a card waits, an
utterance is answered with a fixed reminder ("That change is waiting on your
screen…"), recorded in the durable log as an utterance, and never sent to the
model — so "yes, do it" neither approves the card nor, as a typed message
would, abandons it. `action.resolve` (a button) resolves it with the call's
authenticated caller, and the resumed turn is spoken like any other.

**Metrics.** Every `response.done` carries `metrics`: `endpoint_ms` (last
voice → end of turn), `stt_ms` (end of turn → transcript; near 0 when the
early transcription had finished), `llm_first_text_ms`, `tts_first_ms` (first
piece → its audio), `speech_end_to_first_audio_ms` (the headline: last voice
→ first reply audio sent), `tool_rounds` and `filler`. The same line is logged
under `vogt::call`. `scripts/call_latency.py` measures a deployment with a
WAV file and no microphone.

### Quick chat APIs

A **quick chat** (WI-1097) is a persistent text conversation with an agent
CLI, run without a terminal. The agent is Klaudia, driven over its stream-json
protocol (msp-klaudia `docs/embedding.md`): each message is a `user` line on
its stdin, and its replies, tool calls and the `result` that ends a turn come
back on stdout. The engine picks the conversation id, which is the chat's own
id: `--session-id` on the first launch and `--resume` on every later one. A
chat's process is therefore disposable. It is stopped after
`ENGINE_CHAT_IDLE_AFTER`, and on the next message it is relaunched with the
whole conversation. Chats are kept for good in `state_dir/chats.db`. There is
no retention sweep, and archiving only hides a chat from the default list.

Every route needs the `sessions` capability, because a chat starts an agent
and its transcript is a shared record. They all answer 404 when chats are off
(`ENGINE_CHAT_ENABLED=0`, or no Klaudia launch configured), and `/api/config`
advertises `chat: {drivers: [{name, label, models}]}` only when they are on.
The core's `chat.*` operations are the MCP/CLI/REST counterparts.

- `GET /api/chats?q=&archived=false|true|all&limit=` -> `ChatSummary[]`, newest
  first. `q` is full-text search over titles and over what people and the
  agent said. Tool inputs and results are not indexed.
- `POST /api/chats` `{title?, model?, message?, work_item?}` -> `ChatSendResult`.
- `GET /api/chats/:id?tail=` -> `ChatDetail`, i.e. the summary plus `entries`
  (`user`, `assistant`, `tool-call`, `tool-result`, `notice`, `error`,
  `approval`) and `approvals` still pending.
- `POST /api/chats/:id/messages` `{text, wait_secs?}` -> `ChatSendResult`.
  `wait_secs` (≤ 300) waits for the turn to end. A promoted chat answers 409.
  An `error` entry with `retryable: true` (a provider refusal mid-turn,
  WI-1007, or a stopped agent) is answered by sending the message again.
- `POST /api/chats/:id/approvals/:approval_id` `{allow, message?, person?}` ->
  `ChatApproval`. **Only a person may answer** (the WI-983 rule,
  `person_gate::is_person`). Anyone else gets `403 person required`.
- `POST /api/chats/:id/model` `{model}`: a configured id, or `default`. It takes
  effect from the next turn (`set_model` to a running agent, `--model` on a
  relaunch).
- `POST /api/chats/:id/interrupt`: stops the running turn.
- `POST /api/chats/:id/archive` `{archived}`.
- `POST /api/chats/:id/promote` `{cwd?, name?, template?, work_item?}` ->
  `{chat, session}`. This stops the chat's agent and starts a terminal session
  from the chat's template with `resume: <chat id>`. The session continues the
  same conversation with the session's own credentials. The chat takes no
  further messages.
- `GET /api/chats/:id/events`: an SSE stream of `ChatEvent` (`entry`,
  `progress`, `approval`, `chat`, `lagged`). `/api/events` also carries a
  thin `chat-changed {id}`.
- `POST /api/chats/:id/gate` is **outside the bearer gate**. Only the chat's own
  agent calls it (below), with the per-process token the engine gave that
  agent.

**What a chat's agent can do.** A chat reads pages and documents nobody
vetted, so the engine assumes its agent can be talked into anything:

- **Its environment holds no secrets.** The agent starts from a cleared
  environment plus an allowlist (`PATH`, `HOME`, locale, `TZ`, proxy and CA
  variables), its gate's URL and token, and the one provider key that
  `ENGINE_CHAT_PROVIDER_KEY` names. The engine resolves that key through the
  agent-auth manifest's `get` when the deployment brokers it, or else from its
  own environment. The template's credential wrapper is dropped, so a chat has
  no Vogt token, no brokered service tokens and no secrets-manager identity.
  **Its MCP servers therefore start without credentials. The POC has no MCP
  in chats (decision D1 on WI-1097).**
- **Only its own directory is free.** Each chat runs in
  `state_dir/chats/<id>/`, whose `.klaudia/config.toml` (loaded with
  `--trusted-project-config`) declares a `PreToolUse` hook. The hook posts
  every tool call to the gate. The free path is an allowlist, and it fails
  closed. A call runs at once only in two cases: it is a tool that touches
  nothing outside the agent (`ToolSearch`, `TodoWrite` and the like), or it is
  a read tool the engine knows argument by argument (`Read`, `Glob`, `Grep`
  and the LSP tools, from Klaudia's own schemas) whose every argument is one
  it knows and whose every path resolves, lexically and through symlinks,
  inside that directory. An unknown tool, an unknown argument, or a missing
  or non-string path is a card. **Everything else waits on an approval card
  that a person answers.** That includes any read elsewhere (`/proc/self/environ`, the
  engine's state, `~`), every web, browser and MCP call (**decision D2:
  every web call is carded**), every command and every edit. A prompt
  injection therefore cannot read something and send it out in one unseen
  step, because each half is a card naming what it would do. Klaudia's own
  `can_use_tool` asks (host changes) become cards too. `ask_user` and
  `exit_plan` are answered "not supported in a chat".
- **The gate fails closed where it can.** On any failure to get an answer, the
  hook exits 2, which is Klaudia's "block". The engine denies a card at
  `ENGINE_CHAT_APPROVAL_TIMEOUT`, which is before curl gives up and before the
  hook's own timeout, after which Klaudia would let the call through. When
  Klaudia asks whether the chat's hooks may run, the engine allows that only
  for its own file, byte for byte as it wrote it. If a gated call succeeds
  without the gate having seen exactly that call, the chat's agent is stopped
  with an error. The gate is a driver's gate, not a sandbox. A command a person
  allows runs unconfined, which is what WI-982's uid separation is for.
- **It is bounded.** Limits on chat processes:
  - at most `ENGINE_CHAT_MAX_PROCESSES` (4) at once, and
    `ENGINE_CHAT_MAX_PER_CREATOR` (2) per person; a launch first stops the least
    recently active idle chat, and gets a 409 when every running chat is busy;
  - a turn past `ENGINE_CHAT_TURN_TIMEOUT` (20m) is stopped, and killed if it
    does not stop;
  - a process tree over `ENGINE_CHAT_MAX_RSS` (2GiB) is killed;
  - each process group is killed when its agent exits, and at boot the engine
    ends any that a previous process left running (a pidfile with the start
    time, so a reused pid is never hit).

  `/api/status` reports `chats: {running, rss_bytes}`. Chats are not sessions:
  they appear in no session list and never hibernate.
- **What is kept** is text: what was said, tool calls (600 characters), tool
  results (4 000), approvals, notices and errors, with what looks like a
  credential redacted. Klaudia's own JSONL transcript under
  `~/.klaudia/sessions` is what `--resume` continues from.

| Setting | Default | Meaning |
|---|---|---|
| `ENGINE_CHAT_ENABLED` | `1` | `0` turns chats off |
| `ENGINE_CHAT_TEMPLATE` | first Klaudia template | the session template whose driver a chat runs, and that promotion uses |
| `ENGINE_CHAT_COMMAND` | — | the driver command instead (words, or a JSON array) |
| `ENGINE_CHAT_MODELS_JSON` | `[]` | `[{"id","label"}]` for the model picker; the first is a new chat's default; empty offers only the driver's own default |
| `ENGINE_CHAT_PROVIDER_KEY` | — | the name of the provider-key variable the driver reads (its `apiKeyEnv`) |
| `ENGINE_CHAT_IDLE_AFTER` | `10m` | stop an idle chat's process |
| `ENGINE_CHAT_APPROVAL_TIMEOUT` | `10m` | deny an unanswered card |
| `ENGINE_CHAT_TURN_TIMEOUT` | `20m` | stop a turn running longer |
| `ENGINE_CHAT_MAX_PROCESSES` / `_MAX_PER_CREATOR` | `4` / `2` | process caps |
| `ENGINE_CHAT_MAX_RSS` | `2GiB` | kill a chat's process tree past this |

### File APIs

Every path is relative to `workspace_root` and resolved against it by
`workspace_path.rs`, which every filesystem route funnels through: `..`, an
absolute path and a root component are rejected up front, and the canonicalised
result must still start with the root, so a symlink pointing outward is `400`
too. Paths come back relative to the same root, so a client never learns the
absolute layout.

Every read below — `dir`, `tree`, `files`, `files/download`, both searches
and the four git reads — requires `sessions`, so a `read`-only device token or
a zero-scope credential is refused with `403` before the handler runs. The
workspace is a shared record of every session's work, gated like the
scrollback and history that describe it.

Confinement decides where a path may point; `workspace_path::may_show`
decides whether its bytes may leave the engine. Every route that returns file
content — `GET /api/files`, `/api/files/download`, `/api/search` hits and the
working-tree side of `/api/git/diff` — applies it to the **resolved** path and
answers `400` for any hidden component below the root (`.git/`, `.ssh/`,
`.claude/`, `.env`, `.mcp.json`: what `dir`/`tree` already hide) or a
credential name (`*.env`, `*.key`, `*.pem`, `*.tfstate`, `*.tfvars`,
`secrets.*`, `credentials*`, `*_token`, `id_rsa*`, backups of any of these, and
similar). `move` and `duplicate` refuse such a source, so a rename cannot walk
one past the check. The engine refuses to start when `workspace_root` is `/`,
contains `$HOME`, or contains `state_dir`.

- `GET /api/dir?path=` -> `FileEntry[]` — one directory, directories first
  then case-insensitive alphabetical. Dotfiles are omitted; a client that
  wants them lists the hidden directory by name.
- `GET /api/tree?path=&depth=` -> `TreeNode[]` — `depth` is capped at 3 and
  defaults to 0 (children only). Symlinks are skipped entirely, because
  following one is how a walk leaves the workspace.
- `GET /api/files?path=` -> `FileRead` — accepts a workspace-relative path
  or an absolute one under the root. Refuses anything over 5 MiB with `400`,
  enforced on the bytes read rather than an earlier `stat`. A file with a NUL byte in its first 8 KiB is returned as
  `content_base64` with `is_binary: true`; otherwise as `content`, with
  invalid UTF-8 replaced by U+FFFD rather than refused.
- `PUT /api/files` `WriteReq` -> `WriteFileResponse` (requires
  `filesystem-write`) — `content` or `content_base64`, and `create_parents`
  to mkdir the parent first. A bad base64 body fails before any directory is
  created.
- `PUT /api/files/upload?path=&create_parents=&if_match=` (raw body) ->
  `WriteFileResponse` (requires `filesystem-write`) — the streaming upload:
  the body is spooled to disk beside the target and renamed into place.
- `POST /api/files/op` `FileOpReq` -> `{"ok": true, "path"?: string}`
  (requires `filesystem-write`) — one of four operations, tagged by `op`:
  `move`, `delete`, `mkdir`, `duplicate`. An existing destination is `409`,
  as is moving a directory into itself.
- `GET /api/files/download?path=` -> the bytes, streamed, as
  `application/octet-stream` with `Content-Disposition: attachment`. Capped at
  512 MiB — a transfer cap, not a memory one, since the body is streamed.
- `GET /api/search?q=&path=&max=` -> `SearchHit[]` — ripgrep, `max` defaults
  to 200 and is capped at 500. An empty `q` is `400`. `rg` must be on `$PATH`;
  its absence is `500`, deliberately loud, because a silent empty result would
  read as "no matches".
- `GET /api/search/files?q=&path=&max=` -> `FileSearchResult[]` — substring
  match on name and relative path, case-insensitive, `max` defaults to 100 and
  is capped at 500. Ordered by name-prefix match first, then shorter paths.

### Git APIs

`repo` is a workspace-relative path; the handler walks upward from it to the
nearest `.git`, stopping at `workspace_root` so it cannot adopt a repository
outside the workspace. An empty `repo` means the workspace root itself.

- `GET /api/git/status?repo=` -> `GitStatus`. When the path exists but is not
  in a repository this is `200` with `is_repo: false` rather than an error:
  "this directory is not a repo" is an answer a file browser needs to render,
  not a failure.
- `GET /api/git/diff?repo=&path=&staged=` -> `DiffResp` `{path, current, head}`
  — the two texts, not a computed diff; the client renders it. `path` is
  repo-relative and rejected if absolute or containing `..`. A path missing
  from `HEAD` yields an empty `head` rather than an error, which is how an
  untracked file diffs. Credential names and `.git/` are refused on both
  sides; a hidden path inside the repo diffs only when git tracks it
  (`.github/workflows/*`, `.gitignore`); a working-tree link to a refused file
  is refused.
- `GET /api/git/log?repo=&n=` -> `LogEntry[]` — `n` defaults to 50, capped at
  500.
- `GET /api/git/branch?repo=` -> `BranchInfo` `{current, all}`.
- `POST /api/git/op` `GitOpReq` -> `{"ok": true, "branch"?, "commit"?}`
  (requires `git-write`) — one of five, tagged by `op`: `stage`, `unstage`,
  `discard`, `commit`, `checkout`. `commit` returns the new SHA; `checkout`
  returns the branch and takes `create` for `-b`. `discard` restores a tracked
  path from `HEAD` and `git clean`s an untracked one, which means it destroys
  work with no undo.

### GUI APIs

- `POST /api/gui/launch` `{"command": ["..."], "via_sway": bool}` -> `GuiProc`
  `{pid, command, launched_at}` (requires `gui-control`). `command` is argv and
  is **not** passed through a shell, so metacharacters in an argument are
  literal. `via_sway` prefixes `swaymsg exec --` so the process is owned by
  sway and inherits its `WAYLAND_DISPLAY`.
- `GET /api/gui/processes` -> `GuiProc[]` — launched processes still alive.
  Reading the list is also what prunes dead entries from it.
- `POST /api/gui/kill?pid=<pid>` -> `{"ok": true}` (requires `gui-control`) —
  SIGTERM. A pid that no longer exists is success, not `404`: the caller asked
  for it to be gone and it is.

The engine only tracks what it launched. A process started inside a terminal
does not appear here.

### Agent task APIs

Scheduled agent runs. See §7 for the execution model.
Every non-GET route in this group requires `agent-tasks-write`.

- `GET /api/agent-tasks` -> `AgentTask[]`
- `POST /api/agent-tasks` `AgentTaskCreate` -> `AgentTask`
- `GET /api/agent-tasks/:id` -> `AgentTask`
- `PATCH /api/agent-tasks/:id` `AgentTaskUpdate` -> `AgentTask` (every field
  optional; an omitted field is left alone)
- `DELETE /api/agent-tasks/:id` -> `{"ok": bool}`
- `POST /api/agent-tasks/:id/pause` -> `AgentTask`
- `POST /api/agent-tasks/:id/resume` -> `AgentTask`
- `POST /api/agent-tasks/:id/run` -> `AgentTaskRun` — runs now regardless of
  schedule, and returns as soon as the session is spawned. The run's outcome
  arrives later, on the task's `runs`. A human Run Now records `trigger:
  "manual"`. `POST …/run?trigger=api` records `trigger: "api"` instead, and is
  refused with `409` unless the task has an enabled `api` trigger — programmatic
  firing is opt-in.
- `POST /api/agent-tasks/:id/steer` `{text, interrupt?, actor?, reason?}` ->
  `{"ok": true}` — queue a line of steering for the task's in-flight run,
  delivered to its PTY at the next prompt boundary (the idle / waiting-for-input
  state the activity heuristics detect), or forwarded to the configured
  workflow-engine run. `interrupt: true` sends the CLI's cancel (Ctrl-C) before
  the text. Refused with `409` when the task has no run in flight — there is
  nothing to deliver to. Each accepted delivery emits `task.steered` on the
  event stream, carrying the actor and reason.
- `POST /api/agent-tasks/:id/gates/:gate_id/answer` `{option, actor?, reason?}`
  -> `GateRecord` — answer a currently-open approval gate by the index of the
  chosen option. Delivers that option's input to the PTY, or forwards the
  provider-owned gate answer to the configured workflow engine before recording
  the local resolution. This is the only path that resolves a gate to
  *approved*; see §7 for the fail-closed rule.
- `POST /api/agent-tasks/artifacts/cleanup`
  `{"keep_latest_runs_per_task": 10}` -> `PromptArtifactCleanup`
  `{removed_task_dir_count, removed_prompt_file_count,
  removed_context_file_count, removed_session_prompt_file_count,
  removed_bytes}`

`AgentTask.schedule` is tagged by `kind`: `manual`, `interval` with `minutes`,
or `daily` with `times`. `status` is `active` or `paused`. A run carries
`status` (`running`, `completed`, `errored`), the session it spawned, and the
paths of the prompt and context files written for it.

#### Event triggers

A task also carries `triggers: AgentTaskTrigger[]` and a `concurrency` cap
(default 1), on top of its schedule. Each trigger is `{ enabled, kind, …filter
}`, tagged by `kind`:

- `work-transition` — a work item entered `to_state`; optional `project`,
  `item_kind`, `label`, and `work_item` filters. The run is bound to the item
  that transitioned (its ref becomes the run's `vogt_work_item`), matched from
  the core's `work.transitioned` event.
- `observation-new` — a new observed subject of `observation_kind` (optional
  `project`), matched from an `observation.new` core event.
- `drift-proposed` — a drift proposal was raised (optional `project`), matched
  from the core's `drift.raised` event.
- `forge-pr-checks` — a PR's checks reached `status` (`any` | `green` | `red`;
  optional `work_item`), matched from a `forge.pr.checks` core event; the linked
  item is bound to the run.
- `api` — no core event; it arms `POST …/run?trigger=api`.

**How the engine receives events.** It does not poll the core. The front door
already follows vogt-core's `events.list` cursor once
(`vogt_core::spawn_event_follower`) and republishes each change onto the
engine's own event bus as `VogtChanged`, carrying the event's `summary`. The
agent-task **trigger watcher subscribes to that bus** and matches events against
enabled triggers — one subscription, no second cursor, no core credential of its
own. The match reads only the event's own fields (`kind`, `entity_id`,
`summary`); the engine has no view of Vogt's registry, so `work-transition` and
`drift-proposed` filters on project/kind/label depend on the core carrying those
on the event, which it does (`work.transitioned` and `drift.raised`
summaries).

**The rules a fire obeys.**

- *No storm.* A fire that cannot start — the task is at its concurrency cap, or
  a required binding is missing — is **logged and dropped, never retried**. A
  missed fire is accepted (a paused engine misses them too), the way the drift
  push already accepts one.
- *Concurrency cap.* At most `concurrency` runs of a task are in flight at once;
  events are consumed one at a time, and a single event fires a given task at
  most once even if two of its triggers match.
- *Audit.* Every triggered run records a `trigger_detail`
  (`{trigger_kind, event_kind, event_id, event_seq, description}`) naming the
  trigger and the exact event that fired it, and the run emits
  `task.run.triggered` on the stream — so `why` can explain "task ran because
  WI-7 entered ready at seq 4102".

A task may also carry a **Vogt binding** — `vogt_project` (a project slug) and
`vogt_work_item` (a ref such as `WI-7`), both optional, both omitted from the
response when unset, and both settable to `""` through `PATCH` to unbind.
The engine does not resolve either name: it passes them into
each run as `VOGT_PROJECT` and `VOGT_WORK_ITEM`, and names the subject in the
run's prompt file. Resolution belongs to vogt-core, which holds the registry.

A run also carries `findings`: `[{at, text, source}]`, appended whenever the
notify phrase is seen in the run's output. `source` is `notify-phrase`, the
only producer. The push notification is sent as well; the finding is the
durable copy, so that a bound task's report survives a phone that was off and
can be collected as evidence rather than only delivered.

**Approval gates and steering.** A task may declare `gates`: named
approval points, each a first-class step with `question`, `options`
(`{label, input, approve}`), and an optional `timeout_ms`. A run opens them in
order at the prompt boundaries its CLI reaches, holding the PTY at each until it
is answered or fails closed. A run's `gates` is the audit trail: each
`GateRecord` carries its `question`, `options`, and a flattened `state` —
`open` while held, `answered` (with `option_index`, `option_label`, `approved`,
`actor`, `auto`) once a person or the audited bypass chose an option, or
`blocked` (with a `reason`) when it failed closed. A task's `auto_approve` is
the one bypass: with it set, a run answers each gate with that gate's `approve`
option itself, recorded as actor `auto-approve` — a gate with no `approve`
option still fails closed under it. The events `task.gate.opened`,
`task.gate.answered` (with `outcome` `approved`/`blocked`) and `task.steered`
report gate and steer activity on the event stream (see Events and status
above). See §7 for the fail-closed rule and the `--auto-approve` bypass.

When a task uses the optional workflow-engine backend, the same gate and steer
routes remain the client contract. Provider SSE or poll responses are mirrored
into local `GateRecord`s, preserving the provider gate id privately; an answer
is sent upstream first and is only then recorded locally. The generic REST/SSE
field names are the assumed contract documented in
`engine/server/src/workflow_engine.rs`, and an unavailable provider is a
failed run rather than a core outage. A
broken, ended, or idle SSE subscription gets one bounded re-subscription; after
that budget, tracking settles on the poll fallback. The six-hour run deadline
applies across both transports, so an expired stream cannot start another
subscription.

**Typed outcomes and the conclusion record.** When a run ends the engine
records a typed `outcome` on it — one of `succeeded`, `failed`,
`partially-succeeded`, `skipped`, or `blocked` — resolved by a fixed
precedence: a run that stopped at a gate that failed closed is `blocked`; one
whose findings never matched its `output_schema` is `partially-succeeded`; one
that printed the skip sentinel `VOGT_SKIP:` and exited cleanly is `skipped`;
otherwise a zero exit is `succeeded` and a non-zero exit is `failed`. The
coarser `status` (`completed`/`errored`) is carried alongside, derived from
the exit code, for clients that read only that.

Alongside it the run carries a durable `conclusion`: `{started, finished,
duration_ms, outcome, exit_code, retries, branch?, final_sha?, base_sha?,
diffstat?, cost?, findings}`. The git half is computed in the run's workspace —
`branch` is the branch checked out there (or the task's declared `branch`),
`final_sha` its tip when the run finished, and `diffstat` (`{files, insertions,
deletions}`) is `git diff --numstat` from the sha the run started at (the empty
tree for a fresh repo) to that tip. `cost` (`{total_usd?, input_tokens?,
output_tokens?}`) is parsed from a `VOGT_COST:` line the CLI printed — a JSON
object or a bare dollar amount — and is `null` when the CLI reported nothing. A
workspace that is not a git repo simply omits the git fields. The conclusion is
announced on the event stream as `task.run.concluded` (`{task_id, run_id,
session_id, outcome, exit_code, duration_ms, retries, branch?, final_sha?,
files_changed?, insertions?, deletions?, cost_usd?}`), additive to the
`session.killed` a client already sees for the same run.

**Schema-validated findings.** A task may set an `output_schema` (a JSON
Schema) and, optionally, an `output_file` and an `output_schema_max_retries`
(default 2). With a schema set, the engine reads the findings block the CLI
writes — the first fenced ` ```json ` block in its output, or the file named by
`output_file` — and validates it against the schema (a pragmatic subset:
`type`, `required`, `properties`, `items`, `enum`, and the common numeric and
length bounds). On a mismatch it writes a correction line back into the PTY and
awaits the next block, up to the retry budget; when the budget is spent the run
is recorded `partially-succeeded` with `schema_ok: false` and `retries` set to
the re-prompts spent. A run that validates first try has `schema_ok: true` and
`retries: 0`. With no `output_schema` set, findings are free-text (the notify
phrase) and nothing is validated.

`GET /api/status` reports the same artifacts as counts and bytes, so an
operator can see whether a cleanup is worth running before running one.

### Push APIs

- `POST /api/push/subscribe` -> `{"ok": true, "id", "prefs"}` (requires
  `push-write`). The body is a subscription tagged by `kind`, plus an optional
  `label`: `{"kind":"web-push","endpoint","p256dh","auth"}` or
  `{"kind":"fcm","token"}`. The id is a hash of the endpoint or device token,
  so re-subscribing the same device is idempotent and keeps its preferences.
- `POST /api/push/update` `{"id", "label"?, "clear_label"?, "prefs"?}` ->
  `{"ok": true, "id", "label", "prefs"}` (requires `push-write`). `label` and
  `clear_label` are separate because JSON cannot tell "leave it alone" from
  "set it to nothing".
- `POST /api/push/unsubscribe` `{"id"}` -> `{"ok": bool}` (requires
  `push-write`); `false` means there was nothing to remove.
- `GET /api/push/list` -> subscription entries
  `{id, label, created_at, kind, prefs, pending_digest_count,
  pending_digest_since}`. `kind` is `{"kind":"web-push","endpoint_host"}` or
  `{"kind":"fcm"}` — the endpoint URL and the encryption keys never come back
  out, because the host alone is enough to tell two devices apart.
- `POST /api/push/test` `{"title"?, "body"?}` -> `{ok, fail, queued}` (requires
  `push-write`).
- `POST /api/push/flush-digests` -> `DispatchCounts` `{ok, fail, queued}`
  (requires `push-write`) — sends every digest whose quiet hours have ended,
  which a background task also does once a minute.

`PushPreferences` is a per-kind switch —
`{waiting_for_input, errored, idle_stall, agent_task_started,
agent_task_notify, drift, quiet_hours}` — plus
`quiet_hours: {enabled, start_minute, end_minute, utc_offset_minutes, digest}`.
During quiet hours a notification is counted into a digest instead of sent, and
`queued` in a dispatch count is that outcome rather than a failure.

**Not all of them default on.** The set worth a phone interruption —
`waiting_for_input`, `errored`, `drift` and `agent_task_notify` — and nothing
else is on by default, so those four default `true` and `idle_stall` and
`agent_task_started` default `false`. Both remain switchable; the default is
the claim, not the capability. A stored subscription carries every value
explicitly and therefore keeps whatever it was last set to.

`drift` is not something the engine observes itself. It rides the event
follower: that task polls vogt-core's `events.list` cursor and
republishes each change onto the server's own bus as `vogt-changed`, and the
drift watcher subscribes to that bus the way the session watcher subscribes to
activity. So it is silent whenever the follower is — no core configured, no
stack secret configured, or the core unreachable.

It fires only on `drift.raised`, a named kind rather than a `drift.` prefix,
because "and for nothing else by default" has to survive the core growing new
kinds. `drift.resolved` is deliberately not in the set.

It coalesces: the first drift event opens a ten-second window and everything
inside it is counted, so a sweep that raises thirty proposals sends one
notification rather than thirty. Worst-case latency is the follower's
five-second poll plus that window.

**A restart is a hole in this stream, by choice.** The follower's cursor is in
memory and starts from the core's current head, so drift raised while the
engine was down is never republished and never notified. The proposal itself
is not lost — it stays open in the drift inbox until somebody rules on it — so
what a redeploy costs is the interruption, not the work. The alternative, a
second persisted cursor read only by the notifier, buys that back at the price
of a phone that can replay a deployment's history after a restart; a missed buzz
is recoverable by opening the app, and a notification channel someone switched
off is not.

Notifications the engine sends carry `{kind, session_id, url}` in their data,
where `url` is a PWA route (`/#/t/<session-id>`), so a tap lands on the
terminal that raised it.

### Session history APIs

Archived scrollback for sessions. Every route requires history to be enabled —
it is disabled when the store fails to open, and then these routes answer
`404`, exactly as every other unprovisioned feature does (§ the error table
above): an absent feature reads as absent, not as a broken server.

A row is recorded when a session is created (provisional, with `ended_at` and
`exit_code` NULL) and finalized when it exits. Long-lived sessions that never
`exit` are archived on graceful shutdown (SIGTERM/SIGINT) before the process
leaves, and raw logs that predate their index row are backfilled on startup, so
a session need not have exited while the engine was alive to appear here. A row
with a NULL `exit_code` is one whose outcome is unknown (still provisional,
terminated on shutdown, or backfilled); the `unfinished` status filter selects
exactly those.

Sessions live in the engine's memory, so none survives a restart. At startup,
before it creates any session, the engine closes out every row the previous
process left with a NULL `ended_at` — a session SIGKILLed by a redeploy the
shutdown drain never ran for, or lost to a crash: `ended_at` becomes the last
time its raw log was written, `exit_code` stays NULL (genuinely unknown).
Backfilled logs from before the boot are closed the same way. A NULL
`ended_at` therefore always means a session of the running process.

`SessionMetadata.end_reason` says how a row ended: `exited` (the child exited
while the engine watched; `exit_code` is its code), `engine-shutdown`
(archived by the graceful-shutdown drain while still running), `engine-restart`
(closed out at the next startup), or `null` while the session is live and on
rows from before the field existed.

A row also says what the session was (WI-962): `template`, `role` (`worker`
or `oversight`, as last set), `work_item` (WI-998: the item it served, as
last labelled; cleared by an unbind), and `conversation_agent` / `conversation_id`,
the last agent conversation it ran — one the engine launched, or one an agent
typed into its shell reported. The conversation is kept after it ends, so a
lost session can be found and resumed. `resume_template` is worked out when
the row is read: the template that resumes that conversation with the
engine's templates now, or absent when none can. History's *Resume* starts
`POST /api/sessions` with that `template`, `resume: conversation_id`, the
row's `role` and its `work_item`, so the resumed session is bound to the
same item; vogt-core's `session.history_list` carries the same fields for
`session_start`. The columns are added to an existing `history.db` at boot,
empty on older rows.

- `GET /api/history/sessions?limit=&offset=` -> `SessionMetadata[]`; `limit`
  defaults to 50.
- `GET /api/history/search?q=&limit=&include_live=` -> `SearchResult[]` —
  full-text over archived output, ranked; `limit` defaults to 20. Each result
  carries `live` (default false). `include_live` defaults to **true**: on top
  of the archived FTS hits, each running session's scrollback is scanned
  on-demand (the last `history_live_scan_bytes`, ANSI-stripped, same
  AND-of-terms match) and matches are appended with `live: true`, so output
  that has not been archived yet is still found. The combined list is
  held to `limit`. Pass `include_live=false` for archive-only results.
- `GET /api/history/:id` -> `SessionMetadata`
- `GET /api/history/:id/log?tail_bytes=&strip_ansi=` -> `SessionLogPreview`
  `{session_id, text, bytes, total_bytes, truncated}` — the *tail*, 64 KiB by
  default. `truncated` is how a client knows it is not looking at the whole
  run. `strip_ansi` (default false) removes the escape sequences a terminal
  consumes without printing, so `text` is readable plain text; the byte
  counters still describe the raw tail window that was read.
- `GET /api/history/:id/download` -> the whole log, streamed, as an attachment
  named for the session.
- `DELETE /api/history/:id` -> `{"ok": true}`, `404` if it was already gone
  (requires `history-write`).
- `POST /api/history/cleanup` `{"retention_days": 30}` ->
  `{"ok": true, "removed_sessions", "retention_days"}` (requires
  `history-write`).

### The Vogt front door

Two route families proxy to vogt-core, the Python half of the merged product.
It runs beside the engine on loopback and is never published, so everything a
client asks of it arrives here first. `engine/server/src/vogt_core.rs`
is the implementation and argues the decisions.

| Front door | vogt-core | Auth |
|---|---|---|
| `/api/vogt`, `/api/vogt/*` | `/api/*` | inside the gate; the caller's own bearer forwarded (a break-glass token is swapped for the stack secret) |
| `/mcp`, `/mcp/*` | `/mcp` | the client's own core token, forwarded untouched |
| `POST /api/auth/login` | the same path | none — forwarded untouched, the core self-gates |
| `GET /api/install/status`, `POST /api/install/bootstrap` | the same paths | none — forwarded untouched, the core self-gates |

Each family is three routes rather than two because a wildcard segment needs at
least one character: `/api/vogt/` matches neither `/api/vogt` nor
`/api/vogt/{*path}`, and without its own route it would fall through to the
PWA's catch-all.

**`/api/vogt/*` — any method.** Inside the bearer gate, so it carries the same
credential as every other `/api/*` route, and any method other than GET
requires the `vogt-write` capability. What reaches the core is the **caller's
own bearer**: the session or API token the core just resolved for the gate is
forwarded exactly as it was sent, so the core's audit names the person or
agent who acted and never a proxy identity. Nothing is injected and nothing
is paired. The one substitution is the break-glass `ENGINE_TOKEN`, which the
core has never heard of and which is lent the stack secret, so its Vogt calls
are attributed to the stack secret's actor (`VOGT_BOOTSTRAP_CORE_TOKEN_ACTOR`).
A break-glass request on a door with no stack secret configured is refused
here with a `503` naming `primary` and `vogt_core_token`, rather than
forwarded to collect the core's `401`, because the caller did nothing wrong.
A successful `POST /api/vogt/auth/logout` also evicts the bearer from the
identity cache, so the revoked session is refused by the next engine request
rather than served until the cache expires.

The engine's `vogt-write` gate is about which callers may reach the write
plane at all. It is not a substitute for the core's own rules: a reason on
every write, and the scopes carried by the forwarded credential, are still
enforced there — and because the engine's capabilities derive from those same
scopes, the two gates cannot disagree about who may write.

**`/api/auth/login` — the password login.** Outside the bearer gate, because
a browser that holds no session yet is exactly the caller it exists for, and
forwarded untouched: the core owns the credential check, the per-username
throttle and the audit row, and a login attributed to the stack secret would
put the wrong name on the session it mints.

**`/api/install/*` — the first-run wizard's two routes.** Outside the
bearer gate, because a browser that holds no token yet is exactly the caller
they exist for. The core is the sole authority: its bootstrap answers only
while its token store holds no tokens at all and refuses with `install_closed`
afterwards, so the door adds no gate of its own. Nothing is injected — a
bootstrap attributed to the stack secret would put the wrong name on the
first operator — and the caller's own `Authorization`, if any, survives the
hop untouched, exactly as on `/mcp`.

**`/mcp` — any method.** Deliberately outside the bearer gate. The credential
on an MCP request is already a *core* token — an agent's API token, or a
person's session — bound to an actor, and it is forwarded untouched. The
core judges it afresh on the other side of the hop, so resolving it here
first would buy nothing, and rewriting it would replace a real actor with a
shared one and make the core's audit log worse. Responses stream: `/mcp` is streamable HTTP and its replies
are long-lived SSE, so there is no overall request timeout on the hop — only a
two-second connect timeout, which is what protects against a core that is not
listening.

With no core configured, every route in both families answers `503`; a
core that is configured but does not answer is `502`. Both bodies are
`{"error": {"message": "<reason>"}}` with `X-Vogt-Front-Door: engine`, so an
operator reading a failure in a browser console knows which half of the product
refused. Note that this refusal shape nests where the engine's own errors do
not: a client that parses `error` as a string will not read these. The reason
never names the loopback URL or port.

**Vogt's own operations are not documented here.** They are generated from
Vogt's operation registry and described by its OpenAPI document, which the core
serves at `/openapi.json`; the repository's `AGENTS.md` and [`ARCHITECTURE.md`](ARCHITECTURE.md)
are the entry points. Only `/api/*` is mapped through the front door, so that
document is reachable at the core and not through this port. Duplicating any of
it here would create a second description to keep in step with a registry that
is already the authority.

### The embedded PWA

`GET /` and `GET /{*path}` serve the Solid bundle compiled into the binary,
with an SPA-style fallback to `index.html` for unknown paths. The catch-all is
merged last so `/healthz`, `/api/*` and `/mcp` keep priority — a
new route added *after* it would be shadowed by it and answer with the
application shell, which looks like a client-side routing bug rather than a
server one.

Ordering alone is not enough, because it only protects paths that are
*registered*. `/api` is a list of routes rather than a subtree, so anything not
on the list — `/api/openapi.json`, a typo — would otherwise be claimed by the
SPA fallback and answered `200 text/html`. Every machine namespace is
therefore owned to its leaves: `/mcp` by its proxy routes, `/api` by a
last-resort `/api/{*path}` that answers `404 {"error": "not found"}` in the
engine's ordinary error shape. It is outside the bearer gate — a path that
does not exist does not exist for any credential — and static routes and
`/api/vogt/*` still win, so `/api/status` is still a `401` and
`/api/vogt/nonexistent` still reaches the gate rather than the router's floor.
`app::MACHINE_NAMESPACES` is the list, and a test asserts the property over
it: no path under one is ever `text/html`.

### Response conventions

Avoid anonymous `serde_json::json!` blobs for stable routes when a named typed
response will do. Current standard small shapes:

- `OkResponse` -> `{"ok": true|false}`
- `WriteFileResponse` -> `{"ok": true, "bytes": <n>}`

Several routes return `serde_json::Value` rather than a named type — the push
routes, `DELETE /api/history/:id`, `DELETE /api/agent-tasks/:id`; their shapes
are given beside each route above.

### Not covered here

- **Vogt's operations.** See the front door section: the registry and its
  OpenAPI document are the authority for everything under `/api/vogt/`.
- **Field-by-field type definitions.** The Rust types in
  `engine/contract/src/lib.rs` and the handler modules are the exact shapes;
  this file names them and describes the behaviour a client cannot infer from
  a struct.
- **Configuration.** The two static credentials, the core they front, and
  every value named above as a default: §3 above and
  `engine/server/src/config.rs`. `docs/CONFIG.md` is the *core's* configuration
  and does not describe this process.
- **The assistant's tool loop and threat model** — §6 of this file.
- **Agent task scheduling and execution** — §7 of this file.

---

## 6. The assistant

The assistant is a server-side supervisor with read access to every terminal
session and to a curated read-only slice of Vogt, and confirmation-gated
effectors on both — keystroke injection into a PTY, and mutating Vogt
operations. It is designed to be driven by voice from the mobile app
(on-device STT in, `speechSynthesis` out) or by typed messages from any
browser.

Because a dictated turn reaches the model as whatever the on-device recognizer
wrote down — and that recognizer has never heard of the deployment's project
slugs or session names — the assistant is told its input is dictated and given
a `<vocabulary>` note each voice turn: the project slugs the core knows and the
current session names. It reads a garbled sentence against that vocabulary and
the conversation so far, acts on the clearly-likeliest reading or asks one
short question that states it, and never demands the sentence be repeated word
for word. The note is offered only on turns that carry a recognized utterance
(typed turns were not misheard), the project slugs are fetched from the core
once and cached briefly, and the names are wrapped in `<vocabulary>` as
untrusted data like every other cored-derived string.

### Architecture

- `engine/server/src/assistant.rs` — runtime: in-memory conversation, OpenAI-compatible
  tool-use loop against the configured backend, tool dispatch, pending-action
  gate. The loop also writes the durable interaction log as it runs.
- `engine/server/src/assistant_log.rs` — the durable, attributable interaction
  log: an engine-local append-only SQLite file at
  `state_dir/assistant-log.db`, recording both directions — utterance (raw +
  repaired), request, reply, every tool call and result, and every pending
  action's proposal and outcome (`approved`/`denied`/`expired`). Text and
  structure only, never audio. Engine-local so an absent core costs it
  nothing; a failed open degrades to a live-only conversation rather
  than refusing the assistant. Retention (`assistant_log_retention_days`,
  default 30) is enforced on a daily background sweep, so the horizon is a
  configured maximum rather than whatever the last caller passed.
- `engine/server/src/vogt_tools.rs` — the Vogt toolbox: `tools/list` fetched
  from vogt-core's MCP surface and converted to OpenAI function shape, the
  curated slice, credential resolution, `tools/call`, delimiting.
- `engine/server/src/assistant_stream.rs` — streamed chat completions for
  the live call: decodes an OpenAI-compatible `stream: true` event stream
  back into the same message the loop consumes, reporting each text delta as
  it arrives (see *Streamed turns* below).
- `engine/voxcall/` — `voxcall`, the live call's generic pipeline crate, kept
  free of Vogt types so it can be published on its own (its `DESIGN.md` has
  the trait boundary). It holds
  PCM16/WAV framing; voice activity detection (`EarshotVad`, the default,
  wrapping the `earshot` crate, and `EnergyVad`, an adaptive noise-floor
  detector); the turn endpointer (speech started, sustained, pause, resumed,
  end of turn); and the sentence chunker that cuts a streamed reply into
  pieces to speak, with `speakable` to drop the markdown a listener should
  not hear read out. `voxcall::chunk` decides where a turn is cut into
  chunks to transcribe while it is spoken. The pipeline (`voxcall::pipeline`)
  owns turn-taking, the streamed (or whole-clip) transcription, the streamed
  reply spoken a piece at a time, barge-in and the approval invariant.
- `engine/server/src/call.rs` — Vogt's side of the live call: the WebSocket
  route, authentication and the one-call slot, and the `voxcall` providers —
  the assistant runtime as the turn, the speech proxy as STT/TTS, the pending
  card as approvals (see *Live call contract*, §5).
- `engine/server/src/assistant_api.rs` — HTTP surface (see §5).
- `web/src/Assistant.tsx` — PWA tab: transcript, composer, mic (APK only),
  TTS toggle, approve/deny cards, and the Call control (shown when
  `/api/config` reports `assistant_call_enabled`).
- `web/src/callSession.ts`, `callCapture.ts` (+ `public/call-capture-worklet.js`),
  `callPlayer.ts`, `callProtocol.ts` — the PWA side of the live call: the
  echo-cancelled microphone framed to 16 kHz PCM16 by an AudioWorklet (a
  same-origin file, as the CSP requires), the gapless Web Audio queue that
  plays each piece and reports what started, and the socket client (silence
  while muted, so a turn still ends; reconnects up to three times). During a
  call the approval card's buttons go up the call socket, so the outcome is
  spoken.

The runtime only exists when `assistant_api_key` is configured; otherwise the
routes 404 and the PWA hides the tab (`assistant_enabled` in `GET /api/config`).
The Vogt half is independently absent: with no `vogt_core_url`, or with a core
that is not answering, the `vogt_*` tools are simply not offered that turn and
the terminal half works unchanged.

### Streamed turns

A typed turn, and every `/api/assistant/message` request, waits for the whole
reply. The live call (WI-960) cannot: it speaks the first sentence while the
model is still writing the second. Inside the engine a turn can therefore run
*streamed* — the same loop, gate, log and transcript, with two additions:

- **Deltas.** The model is asked with `stream: true` and each piece of reply
  text is reported the moment it arrives, as is the start of every tool round
  (the moment a caller waiting in silence learns it will be a while). A
  provider that refuses the streamed request is asked again without it, and
  one that ignores the flag and answers with plain JSON is read as the whole
  reply it is; either way the turn completes, only without early text.
- **Cancellation.** A streamed turn can be cut short — a call's barge-in.
  While the model is talking, the stream is dropped and the words received so
  far become the reply, flagged `interrupted` in the transcript and in the
  reply. While tools run, the round is allowed to finish, so every tool call
  in the history keeps its result, and the turn stops before the model is
  asked again. A caller that knows the listener heard less than was written
  cuts the flagged reply back to that prefix, so the next turn's model is not
  told it said something nobody heard.

The live call (`/api/assistant/call`, §5) is the consumer; no plain HTTP
route streams.

### Configuring the assistant provider

The assistant talks to **any OpenAI-compatible chat endpoint** — one that
answers `POST {base_url}/chat/completions` with `tools` / `tool_calls`. A
hosted provider, a routing proxy, or a local server all work; the difference
is a URL, a key and a model id. Three settings turn it on:

```bash
# A hosted provider
ENGINE_ASSISTANT_BASE_URL=https://api.openai.com/v1
ENGINE_ASSISTANT_API_KEY=sk-...
ENGINE_ASSISTANT_MODEL=gpt-5.4-mini

# A local OpenAI-compatible server (e.g. llama.cpp, vLLM, Ollama's /v1)
ENGINE_ASSISTANT_BASE_URL=http://127.0.0.1:11434/v1
ENGINE_ASSISTANT_API_KEY=local      # any non-empty value; the key is what enables the feature
ENGINE_ASSISTANT_MODEL=qwen3-coder
```

With no key the assistant is **off**: its routes answer 404 and the PWA hides
the tab. A key with no base URL is a *startup error*, not a silent
default — the engine refuses to guess where a secret should be sent.

> **Environment prefix.** Engine settings are `ENGINE_*`. The prefix is not
> `VOGT_`: that belongs to the core, which shares this process's environment
> in the merged image, and `VOGT_ENGINE_URL`, `_STATE_DIR` and `_TOKEN_FILE`
> are the *core's* settings for reaching the engine.

Every setting, with the TOML key for a `--config` file and its default
(`engine/server/src/config.rs` is the authority):

| Key (TOML) | Env | Default | Meaning |
|---|---|---|---|
| `assistant_api_key` | `ENGINE_ASSISTANT_API_KEY` | unset (feature off) | bearer sent to the chat endpoint; presence enables the assistant |
| `assistant_base_url` | `ENGINE_ASSISTANT_BASE_URL` | none — required once a key is set | OpenAI-compatible base URL (`/chat/completions` is appended) |
| `assistant_model` | `ENGINE_ASSISTANT_MODEL` | `gpt-5.4-mini` | model id sent with every request |
| `assistant_max_tool_calls` | `ENGINE_ASSISTANT_MAX_TOOL_CALLS` | `8` | upper bound on tool-call rounds per user message |
| `assistant_reasoning_effort` | `ENGINE_ASSISTANT_REASONING_EFFORT` | unset | forwarded as `reasoning_effort` (e.g. `minimal`, `medium`) when set |
| `assistant_allow_claude_proxy` | `ENGINE_ASSISTANT_ALLOW_CLAUDE_PROXY` | `false` | send `claude-*` model ids anyway — see below |
| `assistant_profiles` | `ENGINE_ASSISTANT_PROFILES_JSON` | `[]` | additional named providers, a JSON array of profile objects (below) |
| `assistant_default_profile` | `ENGINE_ASSISTANT_DEFAULT_PROFILE` | the implicit `default` | which profile a request that names none runs on |
| `assistant_log_retention_days` | `ENGINE_ASSISTANT_LOG_RETENTION_DAYS` | `30` | horizon of the durable interaction log, enforced by a daily sweep |
| `history_retention_days` | `ENGINE_HISTORY_RETENTION_DAYS` | `30` | horizon for archived session history (FTS index + raw logs), enforced by a daily sweep; `0` keeps forever |
| `history_live_scan_bytes` | `ENGINE_HISTORY_LIVE_SCAN_BYTES` | `262144` | trailing scrollback bytes scanned per live session when a history search sets `include_live` |
| `assistant_stt_base_urls` | `ENGINE_ASSISTANT_STT_BASE_URLS` (comma-separated) | empty (server STT off) | ordered list of OpenAI-compatible `/audio/transcriptions` bases |
| `assistant_stt_model` | `ENGINE_ASSISTANT_STT_MODEL` | `whisper-1` | transcription model |
| `assistant_stt_language` | `ENGINE_ASSISTANT_STT_LANGUAGE` | `en` | ISO language sent with `/audio/transcriptions`; empty leaves detection to the backend. A client of `/api/assistant/stt` may override it per upload with a `language` field (empty asks for detection) |
| `assistant_stt_api_key` | `ENGINE_ASSISTANT_STT_API_KEY` | unset | key for whichever STT entry needs one; a local server needs none |
| `assistant_tts_base_urls` | `ENGINE_ASSISTANT_TTS_BASE_URLS` (comma-separated) | empty (server TTS off) | ordered list of OpenAI-compatible `/audio/speech` bases |
| `assistant_tts_model` | `ENGINE_ASSISTANT_TTS_MODEL` | `tts-1-hd` | speech model |
| `assistant_tts_voice` | `ENGINE_ASSISTANT_TTS_VOICE` | `nova` | voice name; `/audio/speech` requires one |
| `assistant_tts_format` | `ENGINE_ASSISTANT_TTS_FORMAT` | `mp3` | `response_format` requested from `/audio/speech`; the shipped stack sets `wav` for the bundled Piper sidecar, which serves only wav. The engine passes the upstream content type through, so either plays in the PWA |
| `assistant_tts_api_key` | `ENGINE_ASSISTANT_TTS_API_KEY` | unset | key for whichever TTS entry needs one |
| `assistant_speech_attempt_timeout_ms` | `ENGINE_ASSISTANT_SPEECH_TIMEOUT_MS` | `30000` | per-attempt bound on one speech upstream, not on the whole request |
| — | `ENGINE_ASSISTANT_CALL_ENABLED` | on | offer the live call (`/api/assistant/call`) when the assistant, STT and TTS are configured; off makes it 404 |
| — | `ENGINE_ASSISTANT_CALL_END_OF_TURN_MS` | `700` | silence after speech that ends the user's turn (300–5000) |
| — | `ENGINE_ASSISTANT_CALL_BARGE_IN_MS` | `500` | voice needed to stop a reply by speaking over it (100–3000) |
| — | `ENGINE_ASSISTANT_CALL_STT_MODE` | `chunked` | how a call turn is transcribed: `chunked` streams it to the backend in chunks cut at the speaker's pauses while they talk (captions come free); `whole` transcribes whole clips, for a backend that transcribes short clips badly |
| — | `ENGINE_ASSISTANT_CALL_PARTIAL_INTERVAL_MS` | `0` | `whole` mode only: how often a turn is re-transcribed as a live caption; `0` (the default) turns captions off. On a CPU transcriber each partial is another full decode of the whole turn so far, stacked on the eager transcription that already hides the pause, so captions cost latency rather than saving it |
| — | `ENGINE_ASSISTANT_CALL_VAD` | `earshot` | the call's voice detector: `earshot` (neural, pure Rust) or `energy` (adaptive noise floor) |
| — | `ENGINE_ASSISTANT_CALL_FILLER` | `One moment.` | said while the model runs tools before answering; empty for none |

The Vogt half of the assistant needs no key of its own: it uses
`vogt_core_url` — the same core the front door proxies — and the caller's
own bearer, which the gate already resolved (§6.5). A person signed in with a
password, or an agent with an API token, gets the Vogt tools their scopes
allow; the break-glass token gets them through the stack secret it borrows.

The assistant's core client accepts an HTTP(S) `vogt_core_url` with a host
and no embedded username or password. An invalid URL leaves its Vogt tools
unavailable. It does not follow HTTP redirects: configure the final core
address directly. Redirect responses are reported as core HTTP failures.
The destination is operator configuration, not a tool-call argument.

#### Server-side speech

STT/TTS are configured **independently of the chat profile** above — chat may
run through one provider while audio uses another, or a local Whisper.cpp +
Kokoro pair. Each half's base URLs are an **ordered fallback list**, adopting
the semantics of [voicemode](https://github.com/mbailey/voicemode)'s
`VOICEMODE_STT_BASE_URLS` / `VOICEMODE_TTS_BASE_URLS`: entry 1 first, later
entries on a connection failure or non-2xx — local first, cloud fallback. A
half is enabled when its list is non-empty; the key is reused for whichever
entry needs one (the cloud endpoint) and a local entry needs none. A key set
against an empty list is a startup error, as for chat. Each attempt is
bounded by `assistant_speech_attempt_timeout_ms`. When the list is empty or
every entry fails the route answers **404**, so the client falls back to
on-device recognition or typing. Audio is never stored.

Both lists **ship empty**, so `/api/config` never advertises a backend nobody
is running. voicemode's own lists are the paste-in when you want its shape —
e.g. a local whisper server first, then a hosted fallback:

```bash
ENGINE_ASSISTANT_STT_BASE_URLS=http://127.0.0.1:2022/v1,https://api.openai.com/v1
ENGINE_ASSISTANT_TTS_BASE_URLS=http://127.0.0.1:8880/v1,https://api.openai.com/v1
ENGINE_ASSISTANT_STT_API_KEY=sk-...   # used only by the entry that needs it
ENGINE_ASSISTANT_TTS_API_KEY=sk-...
```

The shipped stack bundles the first-party Rust `voice/` sidecar, on by
default, so a fresh install has working speech with no provider. Its native
providers are `whisper-rs` (GGML Whisper) and `piper-rs` over ONNX, loaded
in-process; no executable or shell is involved, and audio is not retained. The
published `vogt-voice` image carries a small, permissively-licensed default
model set baked in (Whisper `base.en`, a public-domain Piper English voice);
its Piper backend answers `wav` and rejects other formats, so the stack sets
`ENGINE_ASSISTANT_TTS_FORMAT=wav`. `/health` gates on the configured models
loading, so a bad model mount keeps the sidecar unhealthy rather than serving
errors. An operator points it at models of their own by overriding
`VOGT_VOICE_STT_MODEL_PATH` / `VOGT_VOICE_TTS_MODEL_CONFIG_PATH` (or the
JSON-argv subprocess adapter) in an image that starts `FROM` the published one.
See [`voice/README.md`](../voice/README.md) for model naming, supported
WAV/WebM/Opus/Ogg input and WAV output limits, placeholders, and the contract.

#### Voice turn contract

Typed assistant turns send `{text, profile}`. Voice turns send `{text, utterance, profile}`, where `utterance` is the raw STT result and `text` is the visible repaired form. The engine stores both values in its durable assistant log. Aborting a request cancels the client fetch and any in-flight STT/TTS work; a cancelled turn is not recorded. If STT or TTS is unavailable (including a 404), the text conversation remains usable and the PWA reports that speech is unavailable so the user can type or read the response.

#### Provider profiles

A **profile** is one named OpenAI-compatible route:
`{name, base_url, api_key, model, reasoning_effort?, allow_claude_proxy?}`.
The flat `assistant_*` keys above become the implicit profile named
`default` whenever `assistant_api_key` is set, so a deployment that never
heard of profiles keeps exactly the behaviour it had and gains a name a
request can say. `assistant_profiles` is *additional*.

```toml
assistant_default_profile = "hosted"

[[assistant_profiles]]
name = "hosted"
base_url = "https://api.openai.com/v1"
api_key = "sk-…"
model = "gpt-5.4-mini"

[[assistant_profiles]]
name = "local"
base_url = "http://127.0.0.1:11434/v1"
api_key = "local"
model = "qwen3-coder"
reasoning_effort = "medium"
```

The same list as an environment variable is a JSON array of the same
objects: `ENGINE_ASSISTANT_PROFILES_JSON='[{"name":"local","base_url":"http://127.0.0.1:11434/v1","api_key":"local","model":"qwen3-coder"}]'`.

`POST /api/assistant/message` takes an optional `profile` naming one. An
unknown name is refused and the configured ones are listed; a profile whose
model this transport cannot serve is refused with **the profile named**
(evaluated per profile — the hang described below is one proxy's property,
not the deployment's). Approving a card resumes on the profile that proposed it: an
approval that continued on another model would hand one conversation's tool
results to a model that never saw it.

The route-level guard fires only when *no* configured profile can answer.
With one profile that is the original rule exactly; with two it is the honest
generalisation — a broken second profile must not stop the history of a
conversation held on a working one from being read.

`/api/config` advertises `assistant_profiles: [{name, model, default}]` and
**never a key or a base URL**: a browser offering the choice needs neither,
and a base URL is an exposure value.

**A Claude subscription is not a profile.** It has no HTTP API to point a
`base_url` at, so the way to spend one is the `Claude Code (protected)`
session template — a session, not the assistant loop.

#### `claude-*` model ids are refused by default

Hosted OpenAI-compatible proxies have been observed to answer GPT models
quickly with correct tool calls while their `claude-*` routes hang. A hang is
the worst failure a chat surface can have, being indistinguishable from
thinking, and a client timeout reports "took too long" for something that was
never going to answer. So a `claude-*` model id on this transport is
**refused rather than avoided by convention**: every assistant route answers
with a sentence naming the model, the transport and the setting that
overrides it.

`assistant_allow_claude_proxy` (per profile, or `ENGINE_ASSISTANT_ALLOW_CLAUDE_PROXY`
for the implicit default) turns the refusal off. It exists because the fault
is a *proxy's* rather than the model's: a deployment whose proxy serves those
routes correctly is entitled to say so and to own the result.

**The loop is OpenAI-compatible only, and that is a decision.** A second,
native transport would buy a choice of vendor rather than a capability; the
hang is the failure worth fixing, and the switch above is the escape hatch.

### Tools

The engine's own four are literals in `assistant.rs` — they are this
process's surface onto its own PTYs:

| Tool | Effect |
|---|---|
| `list_sessions` | id, name, command, activity state, exit code, cwd, created_at for every session |
| `read_session_tail` | last N bytes of a session's scrollback (default 4 KiB, max 16 KiB), ANSI-stripped by default |
| `send_input` | type text (max 4 KiB) into a session's PTY, optional Enter |
| `steer_agent_task` | queue a steer (`{task_id, text, interrupt?, reason?}`) for a task's in-flight run, delivered at its next prompt boundary |

`send_input` pauses for on-screen approval before it types (§6). `steer_agent_task`
does not: unlike `send_input`, which can inject arbitrary bytes into any session,
a steer reaches only a task's own in-flight run, is held until that run is at a
safe boundary, and is audited on the `task.steered` event with the actor
recorded as `assistant`.

#### The Vogt tools are fetched, not written

Vogt's operation registry generates its own MCP tool schemas, and the core
serves them at `/mcp`. At the start of every turn the assistant POSTs
`tools/list` to the core and converts each `Tool` into an OpenAI function:
an MCP `inputSchema` is already JSON Schema, so it is forwarded **verbatim**
rather than restated here. A hand-written copy would be correct exactly once,
and would then drift silently as the registry changed.

- **Naming.** `work.get` → MCP `work_get` → function `vogt_work_get`. The
  `vogt_` prefix keeps the engine's `list_sessions` and Vogt's `session_list`
  from ever being confused, by the model or by the dispatcher.
- **Curation.** The [voice capability matrix](ENGINE.md#651-voice-capability-matrix)
  classifies every registered operation. `vogt_tools.rs` carries the available
  read and approval sets; `tests/test_voice_capabilities.py` rejects missing,
  stale, duplicate, or misclassified operations and any drift between those
  sets and the matrix. MCP still supplies every schema and filters by scope.
  **`inbox.list` and not `notifications`**: "are there any
  notifications?" is a question about *attention*, and the Inbox projection is
  the one that covers all four sources and carries its own coverage. The
  `notifications` operation is GitHub only; offering both would leave the
  model free to answer the general question from a quarter of the sources and
  report the rest as nothing — which a spoken "no notifications" hides
  perfectly. The system prompt states that an uncollected source is not an
  empty one and must be named.
  A curated name the core does not serve — a rename, or a scope this caller
  lacks — is logged at info and skipped. It is never fabricated: a fabricated
  schema is a tool call that fails at the far end for a reason nobody can read.
- **Which is a write is decided here, not there.** The gate must not depend on
  a remote answer, so `mutating` comes from the curated write set rather than
  from anything the core said.
- **Caching.** One entry per credential, keyed by a SHA-256 digest of the core
  token (never the token), expiring after 5 minutes — because `tools/list` is
  scope-filtered at the core and two tokens may legitimately see two different
  lists. `POST /api/assistant/reset` also clears the cache, which is how an
  operator who has just changed a token's scopes sees the effect without a
  restart.
- **A fetch failure is not an error.** No core configured, core down, or an
  unreadable answer, and the Vogt tools are absent for that turn. The
  assistant still watches terminals.
- **Results are capped** at 16 KiB per call and truncated with a marker.

#### Which credential a Vogt call uses

The assistant runs server-side but is always *called* by an authenticated
user. `require_bearer` leaves an `AuthorizedIdentity` in the request
extensions carrying the caller's name and the credential `/api/vogt` would
present for them — their own bearer, or the stack secret for the break-glass
token; `assistant_api.rs` turns that into a `Caller` and hands it to the
runtime. There is no other credential in reach of the tool loop.

| | Credential |
|---|---|
| Read | the caller's own bearer (for the break-glass token, the stack secret) |
| Write | the **approving** caller's own bearer, and nothing else |

Reads and writes use the same credential, and it is always the caller's, so
every write is audited to the person or agent who approved it. There is no
deployment-wide fallback: the only caller with nothing to act as is the
break-glass token on a door with no stack secret, and it is refused by name
with what to configure. A write filed under a shared token names the wrong
actor in an audit row somebody reads months later, and a wrong answer there
is worse than a refusal a user can act on. The credential is taken from the
request that *approved* the action, not from the one that sent the message
that proposed it — when those differ, the rule is about the second.

Front-door capabilities gate reaching the assistant at all (`assistant` on
every mutating assistant route, held by `work.write`, `project.write` and
`admin`). What a given write is *allowed* to do in Vogt is enforced at the
core against the approver's own scopes, which is the same check any other
client of the core gets.


#### 6.5.1 Voice capability matrix

This policy applies equally to typed and spoken assistant requests.
**Voice-readable** means available without confirmation; **confirmation-gated**
means available only after an on-screen approval of the exact payload and a
meaningful caller-supplied reason. **Operator-only** means unavailable through
assistant tools; use the appropriate CLI or product setup surface. It is an
assistant policy classification, not a new registry authorization scope.

Availability also requires the operation in the core's credential-filtered
MCP list and its optional integrations to be configured. Reads and writes
alike use the caller's own credential — for a write, the approver's — and
retain the core's scope checks and audited write/action path; there is no
deployment-wide fallback, so a personal forge read describes the caller's own
account. No new credential scopes are granted. Linked work writes and initiative publication
can affect the forge; the same approval gate and core writeback policy apply.

<!-- voice-capabilities:start -->
| Operation | Voice class | Availability and reason |
|---|---|---|
| `init` | Operator-only | Unavailable: Local instance initialization; no remote MCP tool. |
| `migrate` | Operator-only | Unavailable: Local schema maintenance; no remote MCP tool. |
| `status` | Voice-readable | Available: Report instance identity, schema versions, and row counts. |
| `instance.diagnostics` | Operator-only | Unavailable: Deploy diagnostics (version, digest, readiness, migrations, redacted error log, optional peer probe) are for operators and agents over MCP, not the assistant. |
| `engine.status` | Operator-only | Unavailable: The engine's operational report (build, counts, storage, event lag) is for operators and agents over MCP, not the assistant. |
| `place.metrics` | Voice-readable | Available: Read all bounded shell navigation counts in one response. |
| `connect` | Operator-only | Unavailable: Client and connection configuration belongs to operator setup. |
| `mcp.stdio` | Operator-only | Unavailable: Local process transport; no remote MCP tool. |
| `project.register` | Operator-only | Unavailable: Registers arbitrary host paths; use project setup outside the assistant. |
| `project.create` | Operator-only | Unavailable: Creates host directories and files; use project setup outside the assistant. |
| `project.import` | Operator-only | Unavailable: Clones into host paths and performs bulk import; use project setup. |
| `project.get` | Voice-readable | Available: Fetch one project by slug. |
| `project.list` | Voice-readable | Available: List registered projects, each with whether work.create lands there now. |
| `project.brief` | Voice-readable | Available: The per-repo view: state, work, bugs, version, compliance. |
| `project.update` | Confirmation-gated | Available after approval: Correct a project's declared repo URL or exclusions. |
| `project.transition` | Confirmation-gated | Available after approval: Move a project through its lifecycle states. |
| `work.create` | Confirmation-gated | Available after approval: Create a work item (feature / bug / chore / question). |
| `work.get` | Voice-readable | Available: Fetch one work item with its relations, labels and comments. |
| `work.list` | Voice-readable | Available: List work items with filters; compact summary rows by default. |
| `board.list` | Voice-readable | Available: Read bounded, independently pageable Board cells in one snapshot. |
| `work.update` | Confirmation-gated | Available after approval: Change a work item's fields, assignee, or labels. |
| `work.transition` | Confirmation-gated | Available after approval: Move a work item to another state, validating the edge. |
| `work.relate` | Confirmation-gated | Available after approval: Add a typed relation between two work items. |
| `work.unrelate` | Confirmation-gated | Available after approval: Remove a typed relation between two work items. |
| `work.bind_branch` | Confirmation-gated | Available after approval: Declare the git branch a work item is worked on. |
| `work.comment` | Confirmation-gated | Available after approval: Comment on a work item, attributed to the acting actor. |
| `backlog` | Voice-readable | Available: The ranked backlog, globally or for one project. |
| `bugs` | Voice-readable | Available: Open bugs across every project, ranked. |
| `why` | Voice-readable | Available: Per-input score contributions for one ranked item. |
| `label.create` | Confirmation-gated | Available after approval: Define a label. |
| `label.list` | Voice-readable | Available: List labels. |
| `initiative.create` | Confirmation-gated | Available after approval: Create a cross-project initiative with a ranking weight. |
| `initiative.update` | Confirmation-gated | Available after approval: Correct an initiative's title, body or weight, or close or reopen it. |
| `initiative.list` | Voice-readable | Available: List initiatives. |
| `initiative.publish` | Confirmation-gated | Available after approval: Create or adopt one forge tracking issue per linked repo the initiative spans, each carrying a managed checkbox task list of its member work items. Additive and forward-only; a closed initiative proposes closing its tracking issues, never writes it. |
| `actor.create` | Operator-only | Unavailable: Admin identity provisioning; actors can be listed and assigned by voice. |
| `actor.list` | Voice-readable | Available: List actors. |
| `workflow.list` | Voice-readable | Available: The state machine each work-item kind is governed by. |
| `sweep` | Confirmation-gated | Available after approval: Run collectors over the registered projects. |
| `coverage` | Voice-readable | Available: What has looked at what, and how long ago. |
| `observations.list` | Voice-readable | Available: Raw evidence, including subjects ranked views filter out. |
| `deps` | Voice-readable | Available: Dependency references out of a project, and into it. |
| `observations.prune` | Operator-only | Unavailable: Admin evidence retention and deletion. |
| `suppress` | Confirmation-gated | Available after approval: Exclude an observed subject from ranked views. |
| `suppression.list` | Voice-readable | Available: List suppressions. |
| `suppression.revoke` | Confirmation-gated | Available after approval: Revoke a suppression, returning the subject to ranked views. |
| `work.adopt` | Confirmation-gated | Available after approval: Promote an observed subject into a declared work item. |
| `contract.evaluate` | Operator-only | Unavailable: Evaluates arbitrary host paths; use compliance or approved contract.check on a registered project. |
| `contract.check` | Confirmation-gated | Available after approval: Evaluate the project contract; returns every failing rule. |
| `contract.adopt` | Confirmation-gated | Available after approval: Opt a project into the contract; it is not applied by default. |
| `contract.decline` | Confirmation-gated | Available after approval: Opt a project back out of the contract. |
| `contract.inapplicable` | Confirmation-gated | Available after approval: Declare that a criterion cannot apply to a project, and why. |
| `contract.applicable` | Confirmation-gated | Available after approval: Withdraw an inapplicability declaration. |
| `project.scaffold` | Operator-only | Unavailable: Writes project files; use explicit project setup. |
| `compliance` | Voice-readable | Available: A project's last recorded contract result, with its age. |
| `drift.detect` | Confirmation-gated | Available after approval: Compare declared state against observation; raise proposals. |
| `drift.list` | Voice-readable | Available: Open drift proposals and their evidence. |
| `drift.resolve` | Confirmation-gated | Available after approval: Accept, reject, or contest a drift proposal. |
| `serve` | Operator-only | Unavailable: Local server process management; no remote MCP tool. |
| `session.start` | Confirmation-gated | Available after approval: Open a coding session for a work item or a project. |
| `session.list` | Voice-readable | Available: List coding sessions with their live activity state. |
| `session.stop` | Confirmation-gated | Available after approval: Stop a coding session and revoke the token it ran with (either id form). |
| `agent_cli.list` | Voice-readable | Available: Report the pod's agent CLIs: active, baked and upstream versions. |
| `agent_cli.update` | Operator-only | Unavailable: Installs executable tooling on the host. |
| `session.history_list` | Voice-readable | Available: List archived sessions (history), newest first. |
| `agent_activity.search` | Operator-only | Unavailable: Agent transcript activity is not curated for the voice assistant; read it over MCP, REST or the CLI. |
| `agent_activity.summary` | Operator-only | Unavailable: Agent transcript activity is not curated for the voice assistant; read it over MCP, REST or the CLI. |
| `session.search_output` | Voice-readable | Available: Search session output (live sessions included). |
| `session.log_tail` | Voice-readable | Available: Read the tail of a session's output log, readable. |
| `session.input` | Operator-only | Unavailable: The assistant types into terminals with its own engine tool (`send_input`, approval-gated); not offered twice. |
| `session.screen` | Operator-only | Unavailable: The assistant reads terminals with its own engine tool (`read_session_tail`); for agents over MCP/CLI/REST. |
| `session.last_reply` | Operator-only | Unavailable: The assistant reads terminals with its own engine tool (`read_session_tail`); for agents over MCP/CLI/REST. |
| `session.wait` | Operator-only | Unavailable: Blocks for up to ten minutes; a voice turn cannot wait that long. For agents over MCP/CLI/REST. |
| `session.report_blocked` | Operator-only | Unavailable: An agent's report about its own session; for agents over MCP/CLI/REST. |
| `session.report_unblocked` | Operator-only | Unavailable: An agent's report about its own session; for agents over MCP/CLI/REST. |
| `session.answer` | Operator-only | Unavailable: The assistant answers permission dialogs through its own approval flow; for drivers over MCP/CLI/REST. |
| `session.token` | Operator-only | Unavailable: the session engine's own HTTP call to mint and revoke engine-started agent sessions' credentials; not a voice action. |
| `session.sweep` | Operator-only | Unavailable: An oversight table of every session's screen; for drivers over MCP/CLI/REST and the GUI board. |
| `session.hibernate` | Operator-only | Unavailable: Stops a session's processes; for the GUI and for agents over MCP/CLI/REST. |
| `session.wake` | Operator-only | Unavailable: Starts a hibernated session's processes again; for the GUI and for agents over MCP/CLI/REST. |
| `session.keep_awake` | Operator-only | Unavailable: A pin against the idle policy; for the GUI and for agents over MCP/CLI/REST. |
| `session.set_role` | Operator-only | Unavailable: Nominates the overseeing session; for the GUI and for agents over MCP/CLI/REST. |
| `session.rename` | Operator-only | Unavailable: Renames a session as the GUI does; for the GUI and for agents over MCP/CLI/REST. |
| `session.remove` | Operator-only | Unavailable: Kills and forgets a session as the GUI's Remove does; for the GUI and for agents over MCP/CLI/REST. |
| `session.bind_work` | Operator-only | Unavailable: Declares which work item a session serves; for the GUI and for agents over MCP/CLI/REST. |
| `session.grant_request` | Operator-only | Unavailable: Asks a person to approve a credential for a session; for agents over MCP/CLI/REST. |
| `session.grant_decide` | Operator-only | Unavailable: A person's approval of a grant, made deliberately in the Inbox, never by voice. |
| `session.grant_revoke` | Operator-only | Unavailable: Revoking a grant; for the GUI and for agents over MCP/CLI/REST. |
| `session.grant_list` | Operator-only | Unavailable: Lists grants to sessions; for the GUI and for agents over MCP/CLI/REST. |
| `token.issue` | Operator-only | Unavailable: Issues credentials that must never enter model context. |
| `token.list` | Operator-only | Unavailable: Admin credential inventory. |
| `token.revoke` | Operator-only | Unavailable: Admin credential revocation. |
| `auth.decisions` | Operator-only | Unavailable: Admin security diagnostics. |
| `auth.whoami` | Operator-only | Unavailable: The caller's own identity check; a diagnostic, not a task. |
| `auth.logout` | Operator-only | Unavailable: Ends the caller's session; never something a voice turn may do. |
| `user.create` | Operator-only | Unavailable: Admin identity provisioning; a password must never enter model context. |
| `user.list` | Operator-only | Unavailable: Admin login inventory. |
| `user.set_password` | Operator-only | Unavailable: A password must never enter model context. |
| `user.remove` | Operator-only | Unavailable: Admin identity removal. |
| `backup` | Operator-only | Unavailable: Local backup destination; no remote MCP tool. |
| `restore` | Operator-only | Unavailable: Replaces instance stores; no remote MCP tool. |
| `clone` | Operator-only | Unavailable: Replaces instance stores with another instance's copy; no remote MCP tool. |
| `export` | Operator-only | Unavailable: Writes a host filesystem destination, despite its registry read classification. |
| `import` | Operator-only | Unavailable: Local bulk state import; no remote MCP tool. |
| `forge.onboard` | Confirmation-gated | Available after approval: Read a repository's existing issues, PRs, labels and releases into observations. Changes nothing upstream. |
| `forge.writeback` | Operator-only | Unavailable: Arms future upstream writes; configure forge policy outside the assistant. |
| `forge.link` | Operator-only | Unavailable: Arms upstream truth and migrates existing work; use explicit forge setup. |
| `forge.publish` | Operator-only | Unavailable: Creates a remote repository and pushes local commits; use explicit forge setup. |
| `forge.account_link` | Operator-only | Unavailable: Accepts a secret PAT; credentials must never enter model context. |
| `forge.account_status` | Voice-readable | Available: Whether you have linked a forge account, and as whom. Never returns the token. |
| `forge.account_unlink` | Operator-only | Unavailable: Changes credential fallback and upstream identity; use account settings. |
| `forge.repos` | Voice-readable | Available: List the repositories your linked credential can see, so you can pick which to import (clone + full sync). |
| `forge.import` | Operator-only | Unavailable: Clones into host paths and performs bulk import; use project setup. |
| `forge.actions` | Voice-readable | Available: The ledger of what Vogt has said upstream, and what landed. |
| `events.list` | Voice-readable | Available: Read the cursor-based event feed. |
| `notifications` | Operator-only | Unavailable: GitHub-only view; voice uses inbox.list for complete attention coverage. |
| `deployed.versions` | Operator-only | Unavailable: Not yet in the curated voice set; deploy-lane failures already reach voice through inbox.list. |
| `inbox.list` | Voice-readable | Available: List the normalized attention Inbox with coverage. |
| `inbox.archive` | Confirmation-gated | Available after approval: Archive one normalized Inbox occurrence. |
| `inbox.snooze` | Confirmation-gated | Available after approval: Snooze one normalized Inbox occurrence until a deadline. |
| `inbox.restore` | Confirmation-gated | Available after approval: Restore one archived or snoozed Inbox occurrence. |
| `preference.get` | Operator-only | Unavailable: Per-person UI settings (saved filters); the assistant has no use for them. |
| `preference.set` | Operator-only | Unavailable: Changes a person's saved UI settings; set them in the surface they belong to. |
| `chat.list` | Operator-only | Unavailable: Quick chats are a separate agent (Klaudia) with its own transcript; the assistant is not a client of them. For the Chat panel and agents over MCP/CLI/REST. |
| `chat.get` | Operator-only | Unavailable: Reads a quick chat's transcript; for the Chat panel and agents over MCP/CLI/REST. |
| `chat.create` | Operator-only | Unavailable: Starts another agent; the assistant starts sessions instead. |
| `chat.send` | Operator-only | Unavailable: Talks to another agent; for the Chat panel and agents over MCP/CLI/REST. |
| `chat.decide` | Operator-only | Unavailable: Only a person answers a chat's approval, in the Chat panel; an agent's call is refused. |
| `chat.set_model` | Operator-only | Unavailable: A chat's model is the person's choice in the Chat panel. |
| `chat.interrupt` | Operator-only | Unavailable: Stops a chat's turn; for the Chat panel and agents over MCP/CLI/REST. |
| `chat.archive` | Operator-only | Unavailable: Files a chat away; for the Chat panel and agents over MCP/CLI/REST. |
| `chat.promote` | Operator-only | Unavailable: Turns a chat into a terminal session; for the Chat panel and agents over MCP/CLI/REST. |
| `audit.list` | Voice-readable | Available: Query the audit log. |
| `registry.dump` | Voice-readable | Available: The operation registry as a manifest — every operation's scope, bindings and schemas. |
<!-- voice-capabilities:end -->

### Threat model

Extends the rule §7 states for agent tasks: external content must never
become instructions.

- **Terminal output is untrusted.** Sessions run arbitrary programs, including
  other AI agents consuming untrusted web content. Anything those programs
  print can reach the assistant's context via `read_session_tail`. Tool
  results are wrapped in `<terminal-output>` delimiters and the system prompt
  tells the model to treat embedded instructions as data — but that is
  defense-in-depth, not the guarantee.
- **Vogt content is untrusted too, by the same rule.** A work item's
  title and body are typed by people; an imported GitHub issue's body is typed
  by strangers on a forge; marker text and drift detail are quoted from files
  nobody reviewed for this purpose. Stored data is external content exactly as
  program output is, and it reaches the model's context the moment the
  assistant reads a backlog. So **every** Vogt result — reads, and the core's
  answer to an approved write — is wrapped in
  `<vogt-data operation="…">` delimiters, with the same framing in the system
  prompt: what is inside is data to report on, never instructions to follow.
  The rule is about where the text came from, not about which tool fetched it.
- **The guarantee is structural.** `send_input` is intercepted in the tool
  dispatcher: the loop pauses
  and returns a `PendingAction` carrying the exact bytes and target session.
  Nothing reaches a PTY until `POST /api/assistant/actions/:id` approves it.
  Actions expire after 120 s; one may be pending at a time; a new user
  message auto-denies it. No model output can bypass this, because the model
  never holds the write handle.
- **Vogt writes use the same gate, with no auto-type escape.** A
  mutating `vogt_*` tool is intercepted in the same place and pauses the same
  loop; the pending action carries the operation, a one-line target, the
  exact arguments pretty-printed, and the `reason` that will be written to
  Vogt's audit log. Nothing reaches the core until the action is approved.
  The gate is structural for terminal input and for Vogt alike: there is no
  configuration that lets a model's output reach a PTY or become a Vogt write
  unattended. A write proposed without a
  reason is refused before it becomes a card at all — a card that cannot say
  what will be recorded is not an approval anyone can meaningfully give — and
  the dispatcher refuses a mutating Vogt tool outright if it ever reaches it,
  so a future edit that loses the interception fails closed instead of
  writing.
- **No other effectors.** The assistant has no file, network, git, or config
  tools, and its Vogt reach is the curated slice — no `token.issue`, no
  `restore`, no `import`, whatever else the core may serve. Its blast radius
  is: read scrollback, read that slice of Vogt, and (after approval) type into
  a PTY or make a matrix-approved Vogt write as the approving user.
- **The UI never auto-approves by voice.** Approval is an on-screen tap so a
  misheard utterance can't authorize an injection or a write. The spoken
  announcement says what is being asked and that it must be approved on
  screen; it offers no spoken way to answer.
- **Privacy:** `read_session_tail` output — which may include secrets printed
  in a terminal — and every Vogt read are sent to the configured LLM provider
  and kept in the in-memory transcript (never persisted to disk;
  `POST /api/assistant/reset` or a restart clears it). Don't provision the
  assistant against a provider you wouldn't show your terminals *and your
  work tracker* to.
- **Scoping:** mutating assistant routes require the `assistant` capability,
  which `work.write`, `project.write` and `admin` hold and `read` alone does
  not. A caller holding it transitively gains type-into-any-session through
  approval, which is why a viewer's login is kept out. It does **not** gain
  Vogt write power beyond its own: every Vogt write is made with the
  approver's own credential, and the core enforces that credential's scopes.

### Voice

- **STT** — `@capacitor-community/speech-recognition` (on-device Android
  recognizer) inside the APK, Web Speech in a desktop browser, or the server
  pipeline (`MediaRecorder` → `POST /api/assistant/stt`) when the device has
  neither or prefers it. The composer mic is **tap-to-talk**: a tap opens a
  take, partial results land in the composer, and the take auto-sends when the
  speaker goes quiet (`silence_duration_ms` after the transcript last changed;
  on the server path, an energy endpointer over the capture — speech onset,
  then that much quiet) or when the mic is tapped again. A press held past
  `hold_threshold_ms` (400) is **push-to-talk** instead: the take is exactly as
  long as the hold and ends on release, and a pause mid-hold never ends it. An
  open tap take is bounded — one that hears nothing ends without sending after
  `no_speech_timeout_ms` (8000; the Android recognizer's own no-speech error
  never reaches JS), and every tap take is capped at `max_turn_ms`. The
  button works from the keyboard (space/enter tap or hold the same way).
  JS owns the silence detection (the dictation-mode end-of-speech is late and
  untunable), and a grace after the recognizer's own stop includes the final
  result rather than the last interim guess. A press or stop tap that lands
  before the native recognizer has finished starting is honoured once it is
  up, never against a recognizer that is not up yet. `RECORD_AUDIO` is
  declared in the manifest; the plugin prompts at first use.
- **TTS** — Web Speech `speechSynthesis` when the browser has it (the desktop
  PWA), sentence-chunked, toggle persisted in localStorage. The synth is primed
  on the toggle gesture because the Android WebView requires a user gesture
  before the first utterance. The **Android WebView has no `speechSynthesis`**,
  so the APK speaks through the server route `POST /api/assistant/tts` — and
  only when a TTS backend is configured. The engine defaults its speech
  base-URL lists to empty on purpose, so `assistant_tts_enabled` reads false
  rather than advertising a mouth that cannot speak (`config.rs`).
- **Hands-free conversation** — a client-only loop (`web/src/voiceTurn.ts`, a
  pure state machine) that keeps listening between turns: speak → silence →
  send → speak the reply → re-open the mic, no touch between turns. States
  `idle → arming → listening → endpointing → sending → speaking → listening`,
  plus `paused_for_approval` (a pending write is announced, the mic closed
  until the on-screen approve/deny — voice still never approves) and a `muted`
  flag that keeps the session alive. Default is half-duplex (mic closed while a
  reply plays). Turn detection is client-owned on every backend, tuned by
  localStorage in the OpenAI Realtime vocabulary so a Realtime-shaped backend
  adopts it unrenamed: `vogt.assistant.voice.silence_duration_ms` (1000, also
  the tap take's window), `hold_threshold_ms` (400, tap take only),
  `no_speech_timeout_ms` (8000, tap take only),
  `final_result_grace_ms` (300), `max_turn_ms` (30000),
  `idle_timeout_ms` (60000), `max_empty_turns` (3),
  `interrupt_response` (false), `reopen_delay_ms` (400 — settle time before
  the mic re-opens behind the app's own playback; Android can otherwise bring
  the recognizer up silently dead), `mic_watchdog_ms` (8000 — a freshly opened
  mic that reports nothing is restarted), `max_mic_restarts` (2). Requires an event-driven recognizer
  (native plugin or Web Speech) and a TTS path; the server-STT path is excluded
  and the control is disabled with its reason.
- **Barge-in** (opt-in, `interrupt_response=1`) — the speaker can talk over a
  playing reply: while a reply plays, an echo-cancelled `getUserMedia` capture
  runs a leaky-accumulator onset detector (`voiceVad.ts`, `vad_threshold` 0.045,
  `vad_onset_ms` 500), and a sustained onset halts the reply (`stopSpeaking`) and
  re-opens the mic to catch the interruption. The onset logic is a pure module,
  unit-tested off frame energies; the capture is a thin Web Audio shell that a
  stronger model (e.g. a WASM Silero VAD) can replace behind the same seam. Off
  by default because the WebView's AEC residual is imperfect and a false trigger
  cuts a reply short; half-duplex is the safe default.
- **Live call** (WI-960; *Live call contract*, §5) — a different mode from the
  two above: the engine listens continuously and owns turn-taking and
  barge-in. On the Android app, placing a call also switches the phone's audio
  to its communication mode (`call-start` / `call-end` over the voice bridge,
  `CallAudio.java`), which puts the platform's acoustic echo canceller on the
  WebView's microphone, and routes the reply to the loudspeaker unless a
  headset is connected. What it changes it records and restores when the call
  ends or the activity is destroyed. The shell needs `MODIFY_AUDIO_SETTINGS`
  for this, so it takes an APK update; an older shell ignores the ops and the
  call still works, with the WebView's own echo cancellation only.

There is no setting that lets the assistant type without asking. The
convenience it would buy a trusted single-user setup is outweighed by what it
would cost the sentence the threat model opens with — no model output reaches
an effector without an on-screen act — being true of every tool in every
deployment.

---

## 7. Agent tasks

The engine's scheduler for long-lived or recurring agents — price monitors,
recurring workspace checks, anything a person wants run on a clock rather
than by hand.

### What a task is

- `GET/POST/PATCH/DELETE /api/agent-tasks` adds a durable scheduled-agent
  registry under `state_dir/agent-tasks.json`.
- `POST /api/agent-tasks/:id/run` launches a real PTY session through the
  existing `SessionRegistry`, so WebSocket attach, scrollback, history, auth,
  and push behavior stay on the existing path.
- The PWA exposes a dedicated Tasks tab with create/edit/pause/resume/run/delete
  actions, recent-run inspection, and open-session actions for task runs.
- Task runs persist explicit `running` / `completed` / `errored` status plus
  `completed_at`, `exit_code`, and a short summary derived from the linked
  session exit event.
- Every task run writes a prompt file under
  `state_dir/agent-task-prompts/<task-id>/<run-id>.md` plus a persistent
  `context.md` file. Agent commands receive those paths through environment
  variables and can also use `{prompt_file}` / `{context_file}` placeholders.
- Sessions created with a `prompt` (a work-item brief) share that
  mechanism: their brief is written to
  `state_dir/agent-task-prompts/sessions/<session-id>.md` and exported as the
  same `VOGT_ENGINE_AGENT_TASK_PROMPT_FILE` variable, so one prompt root exists
  and `POST /api/agent-tasks/artifacts/cleanup` accounts for everything under
  it. See `engine/server/src/prompt_files.rs`. An agent CLI started with a
  brief is also given a one-line first prompt naming the file (§5,
  `agent_cli.rs`); a task run's own command is untouched, because it already
  says how to use its prompt file.
- Tasks can schedule `manual`, `interval`, or UTC `daily` runs. The first useful
  product-monitor shape is `interval { minutes = 720 }` for twice daily.
- The default notification hook is output-driven: if an agent prints a line
  beginning with the task's notify phrase (`notify_on_phrase`, default
  `VOGT_NOTIFY:`), the server fans out a push notification linking back to
  that run's session.
- The same line is **recorded on the run** as a finding — `{at, text,
  source}` on `AgentTaskRun.findings` — before the push is sent. A push is a
  delivery, not a record: if nothing was subscribed, or the phone was off, or
  the push service was down, the finding is still there. The push is sent as
  well; the finding is the durable copy beside it.
- A task may name a **Vogt subject** it is about: `vogt_project` (a slug) or
  `vogt_work_item` (a ref like `WI-7`), or both. The engine does not resolve
  either — it exports them into the run as `VOGT_PROJECT` / `VOGT_WORK_ITEM`
  and names the subject in the run's prompt file, so the agent knows what it
  is reporting on. vogt-core's `session-outcomes` collector reads the bound
  tasks and files their runs' findings as observations against that subject.
  Nothing is pushed from here into Vogt's stores: the binding makes the run
  *collectable*, and collection stays Vogt's own act.

Example task payload for a price monitor:

```json
{
  "name": "Widget price monitor",
  "prompt": "Check current online prices for the widget. Compare against prior context. If the best price is lower than the previous best, print exactly: VOGT_NOTIFY: widget dropped to <price> at <store>. Otherwise summarize quietly.",
  "schedule": { "kind": "interval", "minutes": 720 },
  "command": ["codex", "exec", "--prompt-file", "{prompt_file}"],
  "context": "Track best observed price, store, URL, and timestamp here."
}
```

The command is intentionally user-supplied. The engine does not depend on a
specific AI CLI: the published image bakes in `claude` and `codex`, and any
other agent a deployment installs in the pod works the same way.

#### External workflow providers

For a task whose agent loop is owned by a workflow service, add
`workflow_engine` with `engine_url`, a non-empty `workflow` label, and the
optional `token_file`/`repo_ref` fields. Against the generic contract the
engine creates a run with `POST {engine_url}/api/runs`, polls
`GET /api/runs/:id`, follows `GET /api/runs/:id/events` as SSE, answers gates
at `/gates/:gate_id/answer` and steers at `/steer`; the assumed request and
response shapes are documented at the top of
`engine/server/src/workflow_engine.rs`, and a `Bearer` token is sent when
`token_file` is set. Checkpoint branches, approval gates, billing, diff
summary, final commit and terminal status are retained when the provider
reports them. A failed or unreachable provider records the run as errored
and does not break the engine.

### Approval gates and mid-run steering

An agent task runs a CLI in a PTY unattended. Two abilities let a human stay in
the loop without killing the run:

- **Steer.** `POST /api/agent-tasks/:id/steer` queues a line of guidance for the
  task's in-flight run. The engine holds it and delivers it to the PTY at the
  **next prompt boundary** — the idle / waiting-for-input state the activity
  heuristics in `activity.rs` already detect — so guidance lands when the CLI is
  actually waiting, never mid-thought. `interrupt: true` sends the CLI's cancel
  (Ctrl-C) before the text. The queue is drained one item per boundary between
  agent rounds. A steer bar on the Tasks tab and an `steer_agent_task` engine
  tool (offered to the voice assistant, §6) both reach this endpoint; every
  delivery emits `task.steered` with the actor and reason.

- **Approval gates, fail-closed.** A task declares `gates`: named approval
  points, each a first-class step with a `question`, `options`
  (`{label, input, approve}`), and an optional `timeout_ms`. A run opens them in
  order at its prompt boundaries and **holds the PTY** at each — nothing is
  delivered — until it resolves. A person answers with
  `POST /api/agent-tasks/:id/gates/:gate_id/answer {option}`, which delivers the
  chosen option's `input` to the PTY. The guarantee is that a gate **fails
  closed**: one that is interrupted, times out, or whose session dies resolves
  to `blocked`, **never** to an approval. `interrupted != approved` is enforced
  in the type — the only transition into `answered` is an actor choosing an
  option while the gate is still open (`engine/server/src/gates.rs`), and a
  block can only ever write `blocked` and only from `open`, so a late answer
  cannot overturn a block and a death cannot un-approve a real answer. The one
  bypass is a task's `auto_approve`: the run answers each gate with that gate's
  `approve` option itself, audited as actor `auto-approve` on the gate record
  and on the `task.gate.answered` event. A gate with no `approve` option still
  fails closed under the bypass — it is a "yes", not "pick anything". A gate the
  engine restarts on is failed closed on reconcile, because a paused run has no
  orchestrator to hold it and no session to answer into.

### External content is not instruction

A task's output is external content, and so is anything the agent inside it
read to produce that output — web results, issue bodies, notes, another
agent's transcript. None of it may become an instruction to this system. The
assistant enforces the same rule structurally at §6; here it is a property of
what a finding *is*: a recorded observation with a source, never a command.

What a task's findings are *for* is collection — a bound run's
findings are collectable by vogt-core's `session-outcomes` collector, as
evidence with freshness and trust like everything else. Nothing is pushed from
the engine into Vogt's stores; the binding makes the run collectable, and
collection stays Vogt's own act.

---

## 8. What the engine does not do

Named here so the absences read as decisions rather than omissions.

- **It never decides to run anything.** Every session traces to a person, to
  a schedule a person created, or to an event trigger a person armed on a
  task. There is no autonomous work pickup.
- **It is not an IDE.** Monaco is a tab type; there are no language servers in
  the client, no extension ecosystem, and no collaborative editing.
- **It has no editor logic, terminal rendering, or language servers
  server-side.** Those are client concerns, and they stay there.
- **The assistant has no file, network, git or config tools.** Its blast radius
  is: read scrollback, read a curated slice of Vogt, and — after an on-screen
  approval — type into a PTY or make a matrix-approved Vogt write as the
  approving user.
- **There is no native desktop client.** The PWA and the Android shell are
  the clients.

## 9. Optional integrations — what each does when absent

The engine ships client code for a number of external services. Every one of
them is **absent by default**, and its absence is a reported, honest state
rather than a fault — an engine with all of them off is a complete
product. This table is so a reader is also told what each one costs when the
service behind it is not there, and exactly which setting turns it on.

| Integration | What it is for | Turned on by | What its absence means |
|---|---|---|---|
| **Assistant provider** (any OpenAI-compatible chat endpoint) | the assistant's tool-use loop | `ENGINE_ASSISTANT_API_KEY` + `ENGINE_ASSISTANT_BASE_URL` (+ `_MODEL`, profiles) — §6 | The assistant routes answer 404 and the PWA hides its tab. A key with no `_BASE_URL` is a *startup error*, not a silent default. |
| **Speech provider** (OpenAI-compatible audio endpoints) | server-side STT/TTS for the assistant | `ENGINE_ASSISTANT_STT_BASE_URLS` / `ENGINE_ASSISTANT_TTS_BASE_URLS` (+ `_MODEL`, `_VOICE`, `_API_KEY`) — §6 | Each half answers 404 independently; the client falls back to on-device speech or typing. In the shipped stack both are on by default, pointed at the bundled `voice` sidecar (`COMPOSE_PROFILES=voice`; [DEPLOYMENT.md](DEPLOYMENT.md) §5.2). |
| **Vogt core** | the front door, credential checks, the assistant's Vogt tools, the event follower | `VOGT_CORE_URL` + the stack secret, `VOGT_CORE_TOKEN` / `VOGT_CORE_TOKEN_FILE` — §3, §5 | The engine is bootable alone with a break-glass `ENGINE_TOKEN` (with neither it refuses to start): sessions work, `/readyz` stays ready, the Vogt routes answer `503` with a named reason. |
| **FCM** (native push) | push to the Android shell | `ENGINE_FCM_SERVICE_ACCOUNT_FILE` (a path to the Firebase service-account JSON; `ENGINE_FCM_SERVICE_ACCOUNT_JSON`, the document inline, is also accepted but carries a private key in the environment and is warned about) | The FCM transport is disabled; browser web-push still works for any subscription. VAPID keys are generated and persisted under `state_dir`. |
| **GUI streaming** | the GUI tab's live stream of launched processes | `GUI_STREAM_URL` (+ `START_SWAY=1`, and `GUI_STREAM_VERIFIED=1` once an operator has watched it work) | `/readyz` reports `gui: disabled` and the GUI surface's affordances are withdrawn with a stated reason. |
| **Agent CLIs** (`codex`, `claude`, `opencode`, `klaudia`) | agents inside sessions | `INSTALL_AI_CLIENTS=true` at image build (on in the published image), or a user-managed install in the pod's home | Sessions are ordinary shells; the "(protected)" templates cannot start. |
| **Agent service auth** | brokering third-party service credentials (GitHub and others) into a session from a secrets manager | `ENGINE_AUTO_AGENT_AUTH=1` + `ENGINE_AGENT_AUTH_HELPER` naming a helper (the bundled reference helper is auto-selected when its secrets manager's machine identity is present in the environment), configured through the env vars in `agent-auth.sh`'s header | Sessions run without those credentials pre-loaded; nothing in the engine depends on it. **The shipped helper is one pluggable example** — a data-driven reference implementation against one secrets manager; it bakes in no address, project or secret name and is driven by `ENGINE_AGENT_AUTH_SECRETS`/`_PROBES`. A deployment points the variable at a helper of its own that speaks the same subcommands (`check`, `run`, `shell`, `get`, `set`, `fetch`, `store`), or leaves it off. When it *is* on, a session gets exactly the manifest and never the machine identity — see the credential contract below. |

**The agent-auth session credential contract.** A session launched under this
helper gets exactly the manifest — every secret named in
`ENGINE_AGENT_AUTH_SECRETS`, plus the brokered Vogt and GitHub tokens —
resolved once at launch and exported as brokered tokens. The secrets-manager
machine identity that could read the rest of the vault is **deliberately
dropped** before the session's shell starts, so an agent's own "log in to the
secrets manager and fetch a secret" step finds the identity variables empty
*by design*, not because the credential is missing. Two names-only
breadcrumbs mark that boundary for tooling that must tell the two apart:
`AGENT_AUTH_MODE=brokered` and `AGENT_AUTH_GRANTED` — the space-separated
*names* (never the values) of the credentials the session was granted. So any
"look up a secret" agent tooling has to be written against this model: to make
a new secret reachable from sessions, add a line to `ENGINE_AGENT_AUTH_SECRETS`
rather than fetching it ad hoc. Honest caveat: the strip is a boundary between
*processes*, not a kernel one — a same-uid process can still read PID 1's
environment via `/proc/1/environ`, so the identity is out of the manifest's
reach, not off the machine.

**Opting out of the boundary: identity passthrough.** A deployment may
decide the manifest boundary is not earning its keep and hand sessions the full
secrets-manager identity instead. Setting `ENGINE_AGENT_AUTH_IDENTITY_PASSTHROUGH=1`
keeps the secrets-manager identity variables (the ones the shipped helper's
header names as its machine identity) in the session instead of dropping
them, so an agent can log in to the secrets manager and read or write **any**
secret in **any** project directly — no per-secret manifest line, no
fetch/store broker round-trip. The breadcrumb flips to
`AGENT_AUTH_MODE=identity` (from `brokered`) so tooling can branch;
`AGENT_AUTH_GRANTED` is unchanged. Everything else the strip withholds stays
withheld: the engine's own `ENGINE_TOKEN`, the stack secret and the
assistant/STT/TTS keys never reach a session in either mode. The trade-off is
explicit: passthrough removes the containment the manifest was meant to
provide, in exchange for dropping its friction (every new secret otherwise
needs a deployment configuration edit and an engine restart). It is
defensible precisely because of the same-uid caveat above — an agent that
shares the engine's uid can already reach the identity off the host, so in
such a deployment the manifest is cost without a boundary. **Default is
off**: the brokered model stays the norm, and a deployment opts in per stack.
Enabling it is an operator decision, not the engine's default.

**Getting a secret after launch: the on-demand broker.** The one
sanctioned late path is a *manifest-constrained fetch*, and it keeps the
contract above intact: the identity never leaves the engine. A manifest line
may carry a third flag, `ondemand` — `VAR PROJECT_ID SECRET_NAME ondemand` —
which declares `VAR` for sessions but does **not** resolve it at launch. It is
never sitting in the session's environment for the whole session; it exists
there only at the moment it is asked for. The session's environment lists
those names in `AGENT_AUTH_ONDEMAND`, beside `AGENT_AUTH_GRANTED`. A session
asks with `vogt-agent-auth fetch VAR`, which calls the engine's
`POST /api/agent-auth/fetch/{var}` on loopback (`VOGT_ENGINE_BROKER_URL`) with a
per-session broker token the engine minted at spawn (`VOGT_ENGINE_BROKER_TOKEN`)
and revokes when the session is forgotten. The engine — not the helper —
enforces that `VAR` is in `ENGINE_AGENT_AUTH_SECRETS` (any entry, not only
`ondemand`; an undeclared name is refused with the manifest line to add),
rate-limits per session, then runs the configured helper's `get VAR` with the
same re-granted environment a launch gets and returns the value. **Every fetch
is audited** on `vogt::audit` by session id, variable and secret name —
never value. The engine bearer does not open this route, and the broker token
opens nothing else. The engine reads the manifest itself at startup, with the
helper's grammar; a malformed line is a startup error. Same honest caveat as
above: at the same uid this narrows *ambient* exposure and adds an audit
trail; it becomes an enforced boundary only once sessions run as a separate
uid, and the design does not change when that lands.

**Storing a secret from a session: the write broker.** The mirror of the
fetch, for a session that has just *produced* a secret — minted an API token,
generated a keypair, rotated a password it was asked to rotate. Without it the
agent's only options are to hand the value back through a transcript (the
exposure the whole brokered model exists to avoid) or to reach around the strip;
with it there is a sanctioned path that keeps every property above. The
manifest's flag field is a **comma-separated list** drawn from `optional`,
`ondemand` and `writable` — `VAR PROJECT_ID SECRET_NAME ondemand,writable` —
where `writable` is orthogonal to the read policy and means a session may store
that entry. The common case is `ondemand,writable` (a secret a session will
create or rotate, never exported at launch); `optional,writable` covers "create
it once, read it every launch after"; `writable` alone is a rotate-in-place
secret that must already exist. A session sees the names it may store in
`AGENT_AUTH_WRITABLE`, beside `AGENT_AUTH_ONDEMAND`, and asks with
`vogt-agent-auth store VAR`, the value on **stdin** — which calls the
engine's `POST /api/agent-auth/store/{var}` with the same broker token. The
engine authenticates the token, rate-limits (tighter than the fetch — writes are
rarer), checks that `VAR` is declared **and** `writable` (an undeclared name and
a declared-but-not-writable one get distinct refusals naming the manifest edit),
caps the body, then runs the helper's `set VAR` with the value on stdin — never
argv, an env var or a log line — and returns `created`/`updated`. **Every store
is audited** on `vogt::audit` by session id, variable and secret name and
byte length — never value. `writable` widens blast radius only to the entries an
operator pre-authorised, and every overwrite is in the audit log. The same
honest caveat applies.

**Approved grants: one secret, one session, until a time (WI-973).** The
manifest is fixed at deploy time, and changing it cycles the pod. A grant is
the runtime counterpart: a person approves, in the Inbox, one named secret for
one live session, and vogt-core hands it to the engine with
`POST /api/sessions/{id}/grants` `{grant_id, var, project_id, secret_name,
uses, expires_at, reason}`. **Only the stack secret (vogt-core's identity) may
call it**, or `DELETE /api/sessions/{id}/grants/{grant_id}`. The engine decides
that by the credential it compared, not by a name: a core actor whose
`identity_ref` happens to read `vogt-core` is not it. A person's token, the
break-glass token and every session token are refused with 403, even though
they hold `sessions`. The engine refuses:

- a session it does not know (404), or one that has exited (409);
- a deployment with no broker (409);
- a `var` that is a manifest entry (409: a grant never shadows the manifest);
- a *different* grant for a `var` the session can already fetch (409: one
  approval never silently ends another; re-sending the same `grant_id`
  replaces it, which is what a retried approval does);
- a project the manifest does not name and `ENGINE_AGENT_GRANT_PROJECTS` does
  not list (403);
- an expiry in the past or more than 24 h ahead, or a `reason` over 2000
  characters (400).

The session fetches the grant with the command it already has, `vogt-agent-auth
fetch VAR`. The fetch route checks the manifest first, then that session's own
live grants. It runs the helper's `get VAR` with `ENGINE_AGENT_AUTH_SECRETS`
replaced by exactly the granted line, so the helper's manifest check still
holds. A `once` grant is spent by its first fetch, **whether or not the helper
succeeds**: it is taken out of the table before the helper runs and is never
put back, so a revoke or a session exit that lands while the helper is running
stands (a failed fetch means asking again). Each fetch is audited with its
`grant_id`. `vogt-agent-auth grants` (`GET /api/agent-auth/grants`, broker
token) lists the session's live grants with the `reason` each was approved
for, never a value. `GET /api/sessions/{id}/grants` shows the same to an
operator only: the stack secret, the break-glass token or an `admin`-scoped
token; a `work.write` token, which every session holds, gets 403, because
which secrets another session was granted is not every session's to read.

Grants live in memory. The session ending or hibernating, or the engine
restarting, drops them, which fails closed. Every apply and revoke is audited
as `event=session.grant`. The driven-session policy carries one allow rule,
*Approved Vogt Grant*, for credentials a `vogt-agent-auth grants` run in the
session lists, for the use its `reason=` states. The decision half (request,
Inbox, approval by a person only) is vogt-core's `session.grant_*`
([`API.md`](API.md)). The design and its invariants are
[`design/oversight-grants.md`](design/oversight-grants.md).

**What this is not.** "Only the stack secret may apply a grant" is a check on
the credential presented, not a uid boundary. The stack secret is in the
engine's environment and its token file, and sessions run as the engine's
uid, so a session that reads `/proc/1/environ` or that file (the caveat
above) holds it and can apply a grant to itself with no person involved. The
engine warns at start-up (`event=config.token_file_mode`) when a token file's
mode lets a group or others read it; keep it `0600`. Until sessions run as a
separate uid, grants narrow ambient exposure and add an audited,
person-approved path; they are a boundary only once that uid line exists
(WI-982).

Operator-local notes about a particular deployment belong in the git-ignored
`docs/local/`, not here.

The core product's own optional integrations — GitHub collection, MCP, remote
MCP, and the session engine itself — are in
[`CUSTOMISATION.md`](CUSTOMISATION.md), which points here for the engine's.
