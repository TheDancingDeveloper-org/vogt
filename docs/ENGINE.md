# Vogt — The Session Engine

This document describes what the engine *is*, not what it was planned to be.
Where a capability was designed and never delivered, it is not described
here; outstanding engine work is tracked in the GitHub issue tracker and
[`ROADMAP.md`](ROADMAP.md).

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
The `FR-xx` / `NFR-xx` identifiers that appear throughout are stable
requirement ids from the product's requirements baseline, which is not part
of the repository; each rule they label is stated in words beside them.

Companion documents: [`DESIGN.md`](DESIGN.md) (the product's architecture),
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
  WebSocket attach that replays a snapshot before live output (FR-E1).
- **Activity state.** Each session carries `idle` / `running` /
  `waiting-for-input` / `errored`, derived from output heuristics and published
  on a server-wide SSE stream (FR-E2).
- **Agent tasks.** A durable scheduled-agent registry — `manual`, `interval`,
  UTC `daily`, plus **event triggers** that fire runs from vogt-core's own
  state (a work-item transition, a raised drift proposal, a new observation, a
  PR's checks flipping) and an explicit `api` fire — whose runs are real PTY
  sessions, optionally bound to a Vogt project or work item (FR-E7, #290).
- **The assistant.** A server-side tool-use loop over sessions and a curated
  read slice of Vogt, with every effector behind an on-screen approval
  (FR-T1–T4, FR-T6).
- **Workspace-scoped file and git APIs**, a GUI process launcher, web push
  (VAPID and FCM), and archived session history.
- **The front door.** The single published port: the PWA, the engine's native
  APIs, the WebSocket attach path, `/api/vogt` and `/mcp` proxied to the core,
  and aggregate health (NFR-D11).

Two properties hold the merged shape together and are asserted rather than
described. **The core is still the only definition of an operation**: the
PWA's route table resolves against the registry, and the assistant's Vogt tools
are fetched from the core's own MCP `tools/list` rather than written out again.
**The engine is still bootable alone**: with no core configured it serves
sessions, stays ready, and refuses the Vogt routes with a named reason
(FR-E9) — `engine/server/tests/vogt_core.rs` is what says so.

## 2. The tree

```
engine/           its own Cargo workspace — the repository root is not one
  server/         vogt-engine-server crate → `vogt-engine` binary: PTYs, HTTP, WS, SSE, the front door
  contract/       vogt-engine-contract: shared wire DTOs
  Dockerfile      the merged image (context is the repository root)
  Dockerfile.pod  the dev-pod toolchain base the merged image builds on
  deploy/         entrypoint, MCP registration and credential helpers, a
                  sample standalone compose file
web/              the Solid/Vite PWA — the product's GUI, embedded at build time
mobile/           the Capacitor 8 Android shell that loads the deployed PWA
```

The engine's checks run in `.github/workflows/ci.yml` and its image is built
by `.github/workflows/build.yml`.

Crate names, binary and helper names (`mydevenv2-*`), and the legacy
`MYDEVENV2_*` environment prefix still carry a legacy internal name, as do
the Android notification channel ID, browser storage/event keys, and the
`MYDEVENV2_NOTIFY:` task hook. None is presentation copy, and their removal
is pending housekeeping ([`ROADMAP.md`](ROADMAP.md)). Browser/route titles,
install labels, login/errors, notification channel labels and notification
content all use **Vogt**.
Engine settings are read under `ENGINE_*` (see §3); the legacy `MYDEVENV2_*`
names are still accepted as aliases and log a warning at startup.

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

The Dockerfile has four stages: the PWA bundle, the Rust binary with that
bundle embedded, the published core image (lifted whole, so the merged image
runs *the* public core rather than a second build of it — `CORE_IMAGE` has no
default on purpose), and the runtime. The runtime stage starts `FROM` a
**dev-pod base** (`engine/Dockerfile.pod`, published as
`ghcr.io/thedancingdeveloper-org/vogt-pod-base:{lean,full}-*`): an Ubuntu
image carrying the toolchains an agent working *inside* a session is likely to
want — Node, Rust, Java/Gradle, the Android SDK, and in the `full` variant
Flutter. It is a development pod rather than a hardened service image — it
runs as a named user with `sudo`, with a writable home — which is why the
core image at the repository root is still a separate build rather than a
stage of this one: it is the hardened input this image lifts its core from,
not something to deploy on its own (`DEPLOYMENT.md` §1.1).

Build arguments worth knowing, all off by default: `INSTALL_AI_CLIENTS=true`
bakes in the `codex` and `claude` CLIs at the Renovate-pinned versions in
`engine/agent-versions.env` (otherwise
agents are user-managed inside the pod); `INSTALL_CADASTRE_MCP=true` installs
the optional Cadastre MCP bridge (§4); a further provider-specific agent CLI
has its own build arg in the same block. `POD_BASE_IMAGE` selects the pod
base; CI passes it by digest.

The image's entrypoint (`engine/deploy/entrypoint.sh`) supervises the
container: it optionally joins a VPN (`TAILSCALE_AUTH_KEY`, or
`TAILSCALE_AUTH_KEY_FILE` for a compose-secret mount; a node with persisted
tailscaled state rejoins with neither), optionally starts
a headless compositor for the GUI surface (`START_SWAY=1`), starts the Python
core on loopback when `VOGT_CORE_URL` names a loopback address, then execs the
engine as PID 1's child. With `VOGT_CORE_URL` unset the container runs the
engine alone (FR-E9). `engine/deploy/docker-compose.yml` is a minimal sample
for running a *prebuilt* merged image standalone; every deployment-specific
value in it is a `${VAR}` placeholder with no baked address, so it is a
template to fill in, not a file to use as-is. The supported public path is
`deploy/engine.overlay.yml`, which builds the engine from source in front of
the public core image and carries no host paths or secrets-manager assumptions
at all. [`DEPLOYMENT.md`](DEPLOYMENT.md) is the production guide.

The helper scripts the image installs under `/usr/local/bin/mydevenv2-*`:

| Helper | Source | What it does | Needed? |
|---|---|---|---|
| `mydevenv2-entrypoint` | `deploy/entrypoint.sh` | container supervisor, above | yes |
| `mydevenv2-mcp-bootstrap` | `deploy/mcp-bootstrap.sh` | registers Vogt's MCP server (and, opt-in, Cadastre's) with the agent CLIs present in the image | optional — without it an agent registers MCP servers by hand |
| `mydevenv2-vogt-mcp` | `deploy/vogt-mcp-auth.sh` | stdio bridge to Vogt's `/mcp` for clients that cannot take a bearer directly; uses the session's own token | optional |
| `mydevenv2-rust-analyzer-mcp` | `deploy/rust-analyzer-mcp.sh` | starts `rust-analyzer-mcp` anchored to the nearest `Cargo.toml` | optional |
| `mydevenv2-git-askpass` | `deploy/git-askpass.sh` | `GIT_ASKPASS` shim for brokered credentials | optional |
| `codex` wrapper | `deploy/codex-full-access.sh` | runs Codex without its nested sandbox, because the pod is the isolation boundary | only with `INSTALL_AI_CLIENTS` |
| `mydevenv2-agent-auth` | `deploy/agent-auth.sh` | reference `ENGINE_AGENT_AUTH_HELPER`: brokers service credentials from Infisical into a session (`check`, `run -- <cmd>`, `shell`), driven entirely by an env manifest with no baked addresses or secret names | **optional and pluggable** — one example helper; see §9 |
| `mydevenv2-cadastre-mcp` | `deploy/cadastre-mcp-auth.sh` | stdio bridge to a Cadastre MCP endpoint | only with `CADASTRE_MCP_ENABLED=1` |

Nothing in the engine itself depends on `agent-auth`: with
`ENGINE_AUTO_AGENT_AUTH` unset (the default) sessions are plain shells, and
the two "(protected)" session templates that wrap an agent CLI in it simply
fail to start if the helper has nothing to broker.

### 3.2 From source

```bash
# 1. Mint a token (>=16 chars)
export ENGINE_TOKEN="$(openssl rand -hex 24)"

# Optional: scoped tokens and a write-rate cap for the primary token.
# Capability names: sessions, filesystem-write, git-write, gui-control,
# agent-tasks-write, push-write, history-write, history, assistant, vogt-write
# (`sessions` also gates reading a session's detail/scrollback; `history` gates
# reading archived session history — both reads, gated because they expose
# other callers' output.)
export ENGINE_MUTATING_REQUEST_LIMIT_PER_MINUTE=600
export ENGINE_EXTRA_TOKENS_JSON='[
  {"name": "readonly", "token": "replace-with-another-16+-char-secret",
   "capabilities": []}
]'

# 2. Run — from engine/, which is the Cargo workspace root
cd engine
cargo run -p vogt-engine-server -- --bind 127.0.0.1:8910
```

Engine settings are read from `ENGINE_*` environment variables, with the
legacy `MYDEVENV2_*` names accepted as aliases for one release (they log a
deprecation warning at startup). This now includes the three values the CLI
parser owns — the token, the bind address and the config path: they read
`ENGINE_TOKEN`, `ENGINE_BIND` and `ENGINE_CONFIG` (legacy `MYDEVENV2_TOKEN`,
`MYDEVENV2_BIND`, `MYDEVENV2_CONFIG` still work), or the `--token` / `--bind` /
`--config` flags, which win over the environment. Prefer the environment for
the token so it does not appear in process listings.

Optional TOML config, passed with `--config engine.toml`. Precedence is
CLI flags > env > config file:

```toml
bind = "0.0.0.0:8910"
token = "..."                  # or ENGINE_TOKEN
scrollback_bytes = 4194304
default_shell = "/bin/bash"
default_cwd   = "/srv/workspace"
workspace_root = "/srv/workspace"      # default: ~/Working
activity_idle_after_ms = 1500
state_dir = "/var/lib/vogt-engine"     # default: ~/.local/share/mydevenv2
vapid_subject = "mailto:admin@example.invalid"
allowed_origins = [
  "https://vogt.example.com",
  "http://localhost:5173",
  "http://127.0.0.1:5173",
]
auto_agent_auth = false
agent_auth_helper = "/usr/local/bin/mydevenv2-agent-auth"
token_mutating_request_limit_per_minute = 600

[[extra_tokens]]
name = "readonly"
token = "replace-with-another-16+-char-secret"
capabilities = []
mutating_requests_per_minute = 60
```

`capabilities = []` means "authenticated read-only token". Recommended token
policy: keep the primary token as the admin/recovery credential, use a scoped
interactive token for normal browser use, a scoped read-only token for passive
viewers, and a separate `gui-control` token if a browser regularly launches GUI
processes. The PWA's Settings modal stores device-local named auth profiles, so
the primary token need never sit in a browser's `localStorage`.

To run the engine as the front door for a core, set `VOGT_CORE_URL` to the
core's address and `VOGT_CORE_TOKEN` (or `VOGT_CORE_TOKEN_FILE`) to a core
token; per-front-door-token pairings go on `extra_tokens` entries as
`vogt_core_token_file`. The assistant is configured in §6.

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
cd web && pnpm dev   # -> http://127.0.0.1:5173, paste the token into Settings
```

### 3.4 Tests

```bash
cd engine                        # the Cargo workspace root
cargo fmt --check
cargo clippy -- -D warnings
cargo test                       # server unit + integration (HTTP + WS)
cargo test -p vogt-engine-contract # shared wire-contract tests

cd ../web && pnpm typecheck      # PWA TypeScript check
cd ../web && pnpm test           # 75 jsdom tests over the five Vogt surfaces
```

The Python core's suite runs from the repository root and does not need either
toolchain — `NFR-Q6` keeps it that way, and CI runs it with `engine/`, `web/`
and `mobile/` deleted to prove it.

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
reach a Rust LSP, GitHub and Vogt itself without the user wiring anything.
Everything in this section is about the *image*; a from-source engine has
none of it and loses nothing the engine's own routes provide.

**Rust LSP.** `mydevenv2-rust-analyzer-mcp` wraps `rust-analyzer-mcp` and
starts it from the nearest parent directory containing a `Cargo.toml`, which
keeps the analyzer anchored to the active workspace when an agent launches it
from a subdirectory.

```bash
codex mcp add rust-analyzer -- mydevenv2-rust-analyzer-mcp
claude mcp add --scope project rust-analyzer -- mydevenv2-rust-analyzer-mcp
```

Set `MYDEVENV2_RUST_ANALYZER_WORKSPACE` for that MCP entry if a client launches
it from a non-Rust directory.

**GitHub.** `/usr/local/bin/github-mcp-server`, resolved to latest at image
build time. It needs `GITHUB_PERSONAL_ACCESS_TOKEN` — a PAT you supply to the
session's environment (the `agent-auth` helper, where configured, exports one
as `$GH_TOKEN`).

```bash
codex mcp add github -- env GITHUB_PERSONAL_ACCESS_TOKEN=$GH_TOKEN github-mcp-server stdio
claude mcp add --scope project github -e GITHUB_PERSONAL_ACCESS_TOKEN=$GH_TOKEN -- github-mcp-server stdio
```

**Vogt.** A session started for a project or work item registers Vogt's own MCP
server automatically, carrying a per-session actor-scoped token, so an agent's
writes are attributed to that session's actor rather than to a shared identity
(FR-E5, FR-S10). The session exports the endpoint as `VOGT_URL` and
`mcp-bootstrap.sh` registers it; `VOGT_MCP_URL` overrides the endpoint, and
with neither set the bootstrap falls back to the front door on loopback.

**Cadastre — optional, off by default.** Cadastre is a separate product — an
external MCP server an agent in a session may additionally be pointed at —
and is not part of Vogt (NFR-O5). `mydevenv2-mcp-bootstrap` registers it, and
`mydevenv2-agent-auth` fetches its bearer and probes it, only when the
deployment opts in:

| Env | Default | Effect |
|---|---|---|
| `CADASTRE_MCP_ENABLED` | `0` | `1` registers and probes the bridge. At `0` the bootstrap still registers Vogt, and `agent-auth check` reports `skip: Cadastre MCP (optional integration disabled)` rather than failing. |
| `CADASTRE_MCP_URL` | none usable — set it if you enable the bridge | Where the bridge points. The bearer is expected in `CADASTRE_HTTP_TOKEN`, or brokered by `agent-auth`. |

The image installs `cadastre[mcp-client]` only under the
`INSTALL_CADASTRE_MCP` build argument, which is also off by default — so
enabling the flag on an image built without it registers a bridge that is not
there. Both halves are opt-in, deliberately.

---

## 5. The wire contract

This section is the source-of-truth summary for the engine's wire contract.

`engine/server/src/app.rs` holds the route table and is what actually answers
requests; this section describes it route by route. `tests/test_pwa.py` resolves
every engine path in the shipped PWA against both, so the two cannot drift
apart silently (FR-U8). When they do disagree, this section is the one that
is wrong.

### Contract crate

Rust DTOs live in `engine/contract/` (`vogt-engine-contract`); the server is
their only consumer in this tree. Those types cover:

- session lifecycle payloads
- SSE event payloads
- file and git API payloads
- WebSocket attach control frames
- common small response shapes like `{"ok": true}`

The browser client still carries TypeScript mirrors in `web/src/api.ts`, but
those shapes should follow the shared Rust contract instead of ad hoc
server-local structs. The browser/PWA is the supported client surface.

Routes whose response shape is named below without a crate to look it up in are
server-local: the handler's own struct in `engine/server/src/`, named in the
section that documents it.

### Core rules

- All `/api/*` HTTP routes require bearer auth except `/api/config` and
  `/api/push/public-key`.
- `GET /healthz` and `GET /readyz` are public.
- Auth is `Authorization: Bearer <token>`, compared in constant time against
  the primary token and every entry in `extra_tokens`.
- **A valid token is enough for most routes.** Some also require a named
  capability, and those say so; where nothing is said, any valid token will
  do. The capabilities are `sessions`, `filesystem-write`, `git-write`,
  `gui-control`, `agent-tasks-write`, `push-write`, `history-write`, `history`,
  `assistant` and `vogt-write`; the primary token holds all ten, and a scoped
  token holds what its `extra_tokens` entry lists. The mapping lives in
  `required_capability` in `engine/server/src/auth.rs` and is keyed on method
  *and* path, so `GET /api/sessions` needs no capability while
  `POST /api/sessions` needs `sessions`.
- A token that authenticates but lacks the capability gets `403`, not `401`.
  The distinction is worth acting on: `401` means try a different credential,
  `403` means this credential will never work for this route.
- **`gui-control` and `agent-tasks-write` are arbitrary code execution**, equal
  in power to `sessions`: `gui/launch` runs any argv and an agent task runs a
  caller-supplied `command` in a PTY, both as the pod user (which holds
  `NOPASSWD:ALL` sudo). Grant them only where you would grant a shell; do not
  read them as narrower than they are (#519).
- Every mutating request (POST/PUT/PATCH/DELETE) is rate limited per token —
  600 per minute by default, `mutating_requests_per_minute` per scoped token.
  Over the limit is `429` with `Retry-After` in whole seconds.
- **Every** response carries `X-Request-Id`, gated or not, adopted from the
  request when it sent a usable identifier and minted otherwise (r19). One
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
  line worth reading. Verbosity is `RUST_LOG`'s, as it always has been
  (`engine/server/src/observability.rs`).
- Errors are `{"error": "<message>"}` with the status on the HTTP line: `400`
  malformed or out-of-bounds input, `401` no or wrong token, `403` capability
  denied, `404` not found (also: feature not provisioned, see below), `409`
  conflict, `429` rate limited, `500` something the engine owns is broken,
  `502` an optional upstream did not answer.
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
  — seven checks: `workspace_root` (readable directory), `state_dir`
  (writable, proved by writing and removing a probe file), `tailscale`
  (the image's optional VPN join; skipped unless an auth key is provided or
  persisted tailscaled state exists),
  `gui` (skipped unless `START_SWAY`), `vogt_core`, `workspace_agreement` (the core imports inside
  this server's workspace root — FR-E3) and `backup_agreement` (`vogt backup`
  would cover this server's `state_dir` — NFR-I6). `200` when every *fatal*
  check passes, `503` otherwise; the last three are non-fatal by design, so a
  ready container can still be reporting one of them false.
- `GET /api/config` -> `PublicConfig`
  `{gui_stream_url, gui_stream_available, version, features, session_templates, assistant_enabled,
  vogt, assistant_model?, assistant_profiles?}` — what the browser needs at boot,
  before the user has
  typed a token into Settings. It is outside the gate because it returns no
  secrets: `assistant_enabled` is presence only, never the key.
  `features` is read per request from `/etc/mydevenv2/features.json` and is
  `{}` when that file is absent. `gui_stream_available` is the server-owned
  UI gate and is true only when a stream URL, a non-null Selkies feature and
  `GUI_STREAM_VERIFIED=1` are all present. The latter is an operator
  attestation set only after a launched process has rendered through the
  configured stream.
- `GET /api/auth/check` -> `{ok, version?, product_version?, storage?}` — a
  cheap authenticated credential probe. It deliberately does not perform the
  operational checks reported by `/api/status`, so Settings can distinguish a
  valid token from a temporarily unavailable engine without starting a full
  status read.
- `GET /api/push/public-key` -> `{vapid_public_key, fcm_enabled}` — needed to
  call `PushManager.subscribe`, so it must be reachable before a token exists.

`vogt_core` is the only non-fatal readiness check. The core is a separate
process with its own lifecycle: restarting this container would not revive it
and would kill every live PTY, which is exactly what FR-E9 says an absent core
must not cost. So its state is reported in full and left out of the verdict,
and the surfaces that need it say so themselves (FR-U21).

### Session APIs

- `GET /api/sessions` -> `SessionSummary[]`
- `POST /api/sessions` `SessionSpec` -> `SessionSummary` (requires the
  `sessions` capability)
- `GET /api/sessions/:id` -> `SessionDetail`
- `PATCH /api/sessions/:id` `{"name": "..."}` -> `OkResponse` (requires the
  `sessions` capability)
- `POST /api/sessions/:id/kill` -> `OkResponse` (SIGKILL to the child; the
  session stays in the registry so its scrollback is still readable, which is
  what makes this different from `DELETE`. Requires the `sessions` capability)
- `POST /api/sessions/:id/input` `{"text": "...", "submit": bool}` -> `OkResponse`
  (writes verbatim to PTY stdin, 64 KiB cap, `submit` appends `\r`; requires
  the `sessions` capability)
- `DELETE /api/sessions/:id` -> `OkResponse` (requires the `sessions`
  capability)

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

`SessionSpec` also carries optional `model` and `effort` (FR-T11, r16) — which
model the agent CLI in `command` should run, and how hard it should think. The
engine turns them into that CLI's own flags (`agent_cli.rs`):

| Command | `model` | `effort` |
|---|---|---|
| `claude` | `--model <id>` | `--effort <level>` |
| `codex` | `-m <id>` | `-c model_reasoning_effort=<level>` |
| `opencode` | `--model <provider/id>` | *refused — it has no effort control* |

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
  agent binary — `mydevenv2-agent-auth run -- claude` included, which is what
  every protected template looks like.

```json
{
  "name": "scratch/scratch",
  "cwd": "scratch",
  "command": ["mydevenv2-agent-auth", "run", "--", "codex"],
  "model": "gpt-5.6",
  "effort": "medium"
}
```

The text is not passed to the child. The engine writes it to
`state_dir/agent-task-prompts/sessions/<session-id>.md` before the PTY is
spawned and exports that path as `MYDEVENV2_AGENT_TASK_PROMPT_FILE` — the same
variable a scheduled agent task run sets, so an agent started for a work item
and an agent started by a schedule are configured identically. A `prompt` that
is absent, empty, or all whitespace writes no file and sets no variable.

`cwd` is resolved against `workspace_root` and must stay inside it; a path
that escapes via `..` is `400` rather than a shell in `/etc`. `name` is trimmed
and must be non-empty and at most 256 bytes, on creation and on rename alike;
outside that it is `400`, never truncated. Names need not be unique —
duplicates are confusing, not invalid. The remaining `SessionSpec` fields —
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
    "activity": "idle|running|waiting-for-input|errored",
    "exit_code": null,
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

`activity` is a *heuristic*, computed in `engine/server/src/activity.rs` from
the tail of ANSI-stripped scrollback, and a client should render it as a hint
rather than treat it as a fact about the child process:

- `errored` — the child exited non-zero. This one is not a heuristic and wins
  over everything else.
- `waiting-for-input` — the last ~512 visible bytes match a prompt pattern
  (`[y/n]`, a password prompt, a numbered approval menu, a bare `❯`). The
  pattern set is deliberately conservative, because a false positive sends a
  push notification to someone's phone.
- `running` — output arrived within `activity_idle_after_ms`.
- `idle` — no output for longer than that, or none ever.

A session that goes quiet without printing a recognizable prompt therefore
reads as `idle`, not `waiting-for-input`. That gap is what the idle-stall
watcher covers: after `idle_stall_after_ms` of continuous `idle` it sends one
notification, and re-arms only once the session leaves the state. It is
switched **off** for a new subscription — a heuristic about silence is not one
of the four kinds FR-M2 says is worth a phone interruption — so the watcher
runs and dispatches to whoever asked for it and to nobody else.

### Attach protocol

`GET /api/sessions/:id/attach` — WebSocket upgrade. It sits outside the bearer
middleware and does its own auth, because a browser cannot set an
`Authorization` header on a WebSocket handshake. The credential arrives in the
first text frame instead of in the query string, so it does not land in proxy
and access logs. `?token=` still works and is deprecated: it exists only for a
client that has not been redeployed, and it *does* land in those logs.

The token must carry the `sessions` capability — the same one a write needs.
Attaching is a write: the socket's binary frames go to PTY stdin. A read-only
token can list sessions and read a scrollback snapshot over
`GET /api/sessions/:id`, and cannot attach.

The attach sequence is ordered:

1. client sends auth control frame, optionally with its last applied cursor:
   `{"type":"auth","token":"...","resume_from":123}`
2. server sends `snapshot-start`
3. server sends zero or more binary scrollback chunks
4. server sends `snapshot-done`
5. live PTY traffic continues

Client text control frames:

```json
{"type":"resize","cols":120,"rows":40}
{"type":"ping"}
```

Server text control frames:

```json
{"type":"snapshot-start","session_id":"uuid","scrollback_bytes":0,"scrollback_pos":0,"reset":true}
{"type":"snapshot-done"}
{"type":"lag","note":"client too slow; reattach"}
```

When `resume_from` is still retained, `snapshot-start.reset` is false and the
binary snapshot contains only newer bytes. Otherwise the server returns a full
snapshot with `reset` true.

A text frame that does not parse as a control message is treated as raw input,
because some tools send keystrokes as text. Snapshot chunks are capped at
64 KiB each. Close codes a client should recognize: `4408` no auth frame
within five seconds, `4401` bad or missing auth frame, `4404` no such session.

### Events and status

- `GET /api/events` -> `text/event-stream` of `ServerEvent`, one JSON object
  per `data:` line. Variants are `session-created`, `session-renamed`,
  `session-killed`, `activity` (`{id, state, activity_changed_at}`),
  `vogt.changed`, and the agent-task steering
  trio — `task.gate.opened` (`{task_id, run_id, session_id, gate_id, question,
  options}`), `task.gate.answered` (`{…, gate_id, option?, outcome, actor,
  reason?}` where `outcome` is `approved` or `blocked`), and `task.steered`
  (`{…, actor, interrupt, reason?}`) — plus `task.run.concluded` (`{…, outcome,
  exit_code?, duration_ms, retries, branch?, final_sha?, files_changed?,
  insertions?, deletions?, cost_usd?}`, #291), each tagged by `type`.
- `GET /api/status` -> `OperationalStatus` — version, session count, push
  subscription count, live GUI process count, whether the GUI stream and FCM
  are configured, and nested `history`, `agent_tasks`, `auth_broker` and
  `storage` blocks. Storage numbers are counts and byte totals, never paths
  into the workspace beyond the two roots themselves.

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
  **durable** interaction log (FR-T14), newest first. Unlike `history` this is a
  cross-conversation record attributable to each actor and surviving restart, so
  it is scope-gated on the `assistant` capability. Each entry is
  `{seq, at, actor, kind, direction, payload}`, where `kind` is one of
  `utterance` / `request` / `reply` / `tool_call` / `tool_result` /
  `pending_action` / `backend_error`. External content in a payload keeps its
  FR-T4 delimiters. `POST /api/assistant/message` accepts an optional
  `utterance` (the raw recognised text before FR-T13's repair pass) so a
  repaired turn logs both forms.
- `POST /api/assistant/reset` -> `OkResponse`
- `POST /api/assistant/stt` (multipart audio, field `file`) -> `{"text": string}`
  — server-side transcription (FR-T12). Proxies to `/audio/transcriptions` on an
  ordered, independently-configured base-URL list (voicemode semantics: local
  first, cloud fallback). **404** when unconfigured or every entry fails, so the
  client falls back (FR-T6). Scope-gated on `assistant` (a POST under
  `/api/assistant`). Audio is proxied, never stored.
- `POST /api/assistant/tts` `{"text": "..."}` -> an audio stream (`audio/*`) —
  server-side synthesis. Proxies `{model, input, voice}` to `/audio/speech` on
  the same kind of ordered list. **404** when unconfigured/all-failed. Audio is
  streamed back and never stored.

`PendingAction` is tagged by `kind`, because the assistant has two effectors
and a client must not render one as the other:

- `{"kind": "send_input", "id": uuid, "session_id": uuid, "session_name":
  string, "text": string, "submit": bool}` — the exact bytes the assistant
  wants to type into a session.
- `{"kind": "vogt_write", "id": uuid, "operation": string, "target": string,
  "reason": string, "payload": string}` — a mutating Vogt operation (e.g.
  `work.transition`), the arguments pretty-printed in `payload`, and the
  `reason` Vogt will store in its audit log, surfaced on its own because it is
  the part of the approval that outlives it (FR-T2, FR-T3).

Both await approval at `POST /api/assistant/actions/:id`, one at a time. The
identity on *that* request is the credential a Vogt write is made with: the
approving user's paired core token, never a shared one. `GET /api/config`
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

### File APIs

Every path is relative to `workspace_root` and resolved against it by
`workspace_path.rs`, which every filesystem route funnels through: `..`, an
absolute path and a root component are rejected up front, and the canonicalised
result must still start with the root, so a symlink pointing outward is `400`
too. Paths come back relative to the same root, so a client never learns the
absolute layout.

- `GET /api/dir?path=` -> `FileEntry[]` — one directory, directories first
  then case-insensitive alphabetical. Dotfiles are omitted; a client that
  wants them lists the hidden directory by name.
- `GET /api/tree?path=&depth=` -> `TreeNode[]` — `depth` is capped at 3 and
  defaults to 0 (children only). Symlinks are skipped entirely, because
  following one is how a walk leaves the workspace.
- `GET /api/files?path=` -> `FileRead` — refuses anything over 5 MiB with
  `400`. A file with a NUL byte in its first 8 KiB is returned as
  `content_base64` with `is_binary: true`; otherwise as `content`, with
  invalid UTF-8 replaced by U+FFFD rather than refused.
- `PUT /api/files` `WriteReq` -> `WriteFileResponse` (requires
  `filesystem-write`) — `content` or `content_base64`, and `create_parents`
  to mkdir the parent first. A bad base64 body fails before any directory is
  created.
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
  untracked file diffs.
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
  firing is opt-in (#290).
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

#### Event triggers (#290)

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
(`vogt_core::spawn_event_follower`, FR-U10) and republishes each change onto the
engine's own event bus as `VogtChanged`, now carrying the event's `summary`. The
agent-task **trigger watcher subscribes to that bus** and matches events against
enabled triggers — one subscription, no second cursor, no core credential of its
own. The match reads only the event's own fields (`kind`, `entity_id`,
`summary`); the engine has no view of Vogt's registry, so `work-transition` and
`drift-proposed` filters on project/kind/label depend on the core carrying those
on the event, which it now does additively (`work.transitioned` and
`drift.raised` summaries).

**The rules a fire obeys.**

- *No storm.* A fire that cannot start — the task is at its concurrency cap, or
  a required binding is missing — is **logged and dropped, never retried**. A
  missed fire is accepted (a paused engine misses them too), the way FR-M2's
  drift push already accepts one.
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
response when unset, and both settable to `""` through `PATCH` to unbind
(Vogt's FR-E7). The engine does not resolve either name: it passes them into
each run as `VOGT_PROJECT` and `VOGT_WORK_ITEM`, and names the subject in the
run's prompt file. Resolution belongs to vogt-core, which holds the registry.

A run also carries `findings`: `[{at, text, source}]`, appended whenever the
notify phrase is seen in the run's output. `source` is `notify-phrase`, the
only producer today. The push notification is unchanged; the finding is the
durable copy, so that a bound task's report survives a phone that was off and
can be collected as evidence rather than only delivered.

**Approval gates and steering (#289).** A task may declare `gates`: named
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
field names are provisional until a live engine smoke test validates them, and
an unavailable provider remains a failed run rather than a core outage. A
broken, ended, or idle SSE subscription gets one bounded re-subscription; after
that budget, tracking settles on the poll fallback. The six-hour run deadline
applies across both transports, so an expired stream cannot start another
subscription.

**Typed outcomes and the conclusion record (#291).** When a run ends the engine
records a typed `outcome` on it — one of `succeeded`, `failed`,
`partially-succeeded`, `skipped`, or `blocked` — resolved by a fixed
precedence: a run that stopped at a gate that failed closed is `blocked`; one
whose findings never matched its `output_schema` is `partially-succeeded`; one
that printed the skip sentinel `VOGT_SKIP:` and exited cleanly is `skipped`;
otherwise a zero exit is `succeeded` and a non-zero exit is `failed`. The older
`status` (`completed`/`errored`) stays, derived from the exit code as before, so
existing clients are undisturbed.

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

**Schema-validated findings (#291).** A task may set an `output_schema` (a JSON
Schema) and, optionally, an `output_file` and an `output_schema_max_retries`
(default 2). With a schema set, the engine reads the findings block the CLI
writes — the first fenced ` ```json ` block in its output, or the file named by
`output_file` — and validates it against the schema (a pragmatic subset:
`type`, `required`, `properties`, `items`, `enum`, and the common numeric and
length bounds). On a mismatch it writes a correction line back into the PTY and
awaits the next block, up to the retry budget; when the budget is spent the run
is recorded `partially-succeeded` with `schema_ok: false` and `retries` set to
the re-prompts spent. A run that validates first try has `schema_ok: true` and
`retries: 0`. With no `output_schema` set, findings stay free-text (the notify
phrase) and nothing is validated — today's behaviour.

> A run's conclusion is the seam Vogt's `why` scoring (#285) will read for a
> "last task run succeeded/failed 2h ago" input. That wiring is core-Python and
> not yet connected to the engine; the conclusion is exposed on the run and the
> stream so #285 can pick it up without a cross-process link here.

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

**Not all of them default on.** FR-M2 names the set worth a phone
interruption — `waiting_for_input`, `errored`, `drift` and
`agent_task_notify` — "and for nothing else by default", so those four
default `true` and `idle_stall` and `agent_task_started` default `false`. Both
remain switchable; the default is the claim, not the capability. A
subscription stored before this change carries every value explicitly and
therefore keeps whatever it was last set to.

`drift` is not something the engine observes itself. It rides the event
follower (FR-U10): that task polls vogt-core's `events.list` cursor and
republishes each change onto the server's own bus as `vogt-changed`, and the
drift watcher subscribes to that bus the way the session watcher subscribes to
activity. So it is silent whenever the follower is — no core configured, no
core token configured, or the core unreachable.

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

- `GET /api/history/sessions?limit=&offset=` -> `SessionMetadata[]`; `limit`
  defaults to 50.
- `GET /api/history/search?q=&limit=&include_live=` -> `SearchResult[]` —
  full-text over archived output, ranked; `limit` defaults to 20. Each result
  carries `live` (default false). `include_live` defaults to **true**: on top
  of the archived FTS hits, each running session's scrollback is scanned
  on-demand (the last `history_live_scan_bytes`, ANSI-stripped, same
  AND-of-terms match) and matches are appended with `live: true`, so output
  that has not been archived yet is still found (#491). The combined list is
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
client asks of it arrives here first (NFR-D11). `engine/server/src/vogt_core.rs`
is the implementation and argues the decisions.

| Front door | vogt-core | Auth |
|---|---|---|
| `/api/vogt`, `/api/vogt/*` | `/api/*` | front-door token, core token injected |
| `/mcp`, `/mcp/*` | `/mcp` | the client's own core token, forwarded untouched |
| `GET /api/install/status`, `POST /api/install/bootstrap` | the same paths | none — forwarded untouched, the core self-gates (#292) |

Each family is three routes rather than two because a wildcard segment needs at
least one character: `/api/vogt/` matches neither `/api/vogt` nor
`/api/vogt/{*path}`, and without its own route it would fall through to the
PWA's catch-all.

**`/api/vogt/*` — any method.** Inside the bearer gate, so it carries the same
token as every other `/api/*` route, and any method other than GET requires the
`vogt-write` capability. What reaches the core is a *different* credential: the
caller's `Authorization` header is dropped and the core token paired with the
front-door token that authenticated this request is injected in its place
(FR-S9). A deployment with one shared `vogt_core_token` and no per-token
pairings keeps working — that fallback is deliberate, not a leftover — but then
the core's audit names one proxy identity instead of an actor per front-door
holder. A request with neither a pairing nor a fallback is refused here with a
`503` naming the token, rather than forwarded to collect the core's `401`,
because the caller did nothing wrong.

The engine's `vogt-write` gate is about which front-door holders may reach the
write plane at all. It is not a substitute for the core's own rules: a reason on
every write, and the scopes carried by the injected token, are still enforced
there.

**`/api/install/*` — the first-run wizard's two routes (#292).** Outside the
bearer gate, because a browser that holds no token yet is exactly the caller
they exist for. The core is the sole authority: its bootstrap answers only
while its token store holds no tokens at all and refuses with `install_closed`
afterwards, so the door adds no gate of its own. Nothing is injected — a
bootstrap attributed to the deployment's shared pairing would put the wrong
name on the first operator — and the caller's own `Authorization`, if any,
survives the hop untouched, exactly as on `/mcp`.

**`/mcp` — any method.** Deliberately outside the bearer gate. The credential
on an MCP request is already a *core* token, minted by `vogt token issue` and
bound to an actor, and it is forwarded untouched. Re-checking it against the
engine's unrelated token list would refuse every legitimate agent, and
rewriting it would replace a real actor with a shared one and make the core's
audit log worse. Responses stream: `/mcp` is streamable HTTP and its replies
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
serves at `/openapi.json`; the repository's `AGENTS.md` and [`DESIGN.md`](DESIGN.md)
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

Ordering alone was not enough, because it only protects paths that are
*registered*. `/api` is a list of routes rather than a subtree, so anything not
on the list — `/api/openapi.json`, a typo — was claimed by the SPA fallback and
answered `200 text/html` (#34). Every machine namespace is now owned to its
leaves: `/mcp` by its proxy routes, `/api` by a last-resort
`/api/{*path}` that answers `404 {"error": "not found"}` in the engine's
ordinary error shape. It is outside the bearer gate — a path that does not
exist does not exist for any credential — and static routes and `/api/vogt/*`
still win, so `/api/status` is still a `401` and `/api/vogt/nonexistent` still
reaches the gate rather than the router's floor. `app::MACHINE_NAMESPACES` is
the list, and a test asserts the property over it: no path under one is ever
`text/html`.

### Response conventions

Avoid anonymous `serde_json::json!` blobs for stable routes when a named typed
response will do. Current standard small shapes:

- `OkResponse` -> `{"ok": true|false}`
- `WriteFileResponse` -> `{"ok": true, "bytes": <n>}`

Several routes here still return `serde_json::Value` — the push routes,
`DELETE /api/history/:id`, `DELETE /api/agent-tasks/:id`. These are the
standard above not yet applied.

### Not covered here

- **Vogt's operations.** See the front door section: the registry and its
  OpenAPI document are the authority for everything under `/api/vogt/`.
- **Field-by-field type definitions.** The Rust types in
  `engine/contract/src/lib.rs` and the handler modules are the exact shapes;
  this file names them and describes the behaviour a client cannot infer from
  a struct.
- **Configuration.** Which tokens exist, which capabilities they hold, and
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

### Architecture

- `engine/server/src/assistant.rs` — runtime: in-memory conversation, OpenAI-compatible
  tool-use loop against the configured backend, tool dispatch, pending-action
  gate. The loop also writes the durable interaction log as it runs.
- `engine/server/src/assistant_log.rs` — the durable, attributable interaction
  log (FR-T14): an engine-local append-only SQLite file at
  `state_dir/assistant-log.db`, recording both directions — utterance (raw +
  repaired), request, reply, every tool call and result, and every pending
  action's proposal and outcome (`approved`/`denied`/`expired`). Text and
  structure only, never audio (FR-T12). Engine-local so an absent core costs it
  nothing (FR-E9); a failed open degrades to a live-only conversation rather
  than refusing the assistant. Retention (`assistant_log_retention_days`,
  default 30) is enforced on a daily background sweep, so the horizon is a
  configured maximum rather than whatever the last caller passed.
- `engine/server/src/vogt_tools.rs` — the Vogt toolbox: `tools/list` fetched
  from vogt-core's MCP surface and converted to OpenAI function shape, the
  curated slice, credential resolution, `tools/call`, delimiting.
- `engine/server/src/assistant_api.rs` — HTTP surface (see §5).
- `web/src/Assistant.tsx` — PWA tab: transcript, composer, mic (APK only),
  TTS toggle, approve/deny cards.

The runtime only exists when `assistant_api_key` is configured; otherwise the
routes 404 and the PWA hides the tab (`assistant_enabled` in `GET /api/config`).
The Vogt half is independently absent: with no `vogt_core_url`, or with a core
that is not answering, the `vogt_*` tools are simply not offered that turn and
the terminal half works exactly as before (FR-T6, FR-E9).

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
the tab (FR-T6). A key with no base URL is a *startup error*, not a silent
default — the engine refuses to guess where a secret should be sent (r20).

> **Environment prefix.** Engine settings are `ENGINE_*`. The legacy
> `MYDEVENV2_*` names are still accepted as aliases and log a warning at
> startup naming both. The prefix is not `VOGT_`: that belongs to the core,
> which shares this process's environment in the merged image, and
> `VOGT_ENGINE_URL`, `_STATE_DIR` and `_TOKEN_FILE` are already the *core's*
> settings for reaching the engine.

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
| `assistant_stt_api_key` | `ENGINE_ASSISTANT_STT_API_KEY` | unset | key for whichever STT entry needs one; a local server needs none |
| `assistant_tts_base_urls` | `ENGINE_ASSISTANT_TTS_BASE_URLS` (comma-separated) | empty (server TTS off) | ordered list of OpenAI-compatible `/audio/speech` bases |
| `assistant_tts_model` | `ENGINE_ASSISTANT_TTS_MODEL` | `tts-1-hd` | speech model |
| `assistant_tts_voice` | `ENGINE_ASSISTANT_TTS_VOICE` | `nova` | voice name; `/audio/speech` requires one |
| `assistant_tts_api_key` | `ENGINE_ASSISTANT_TTS_API_KEY` | unset | key for whichever TTS entry needs one |
| `assistant_speech_attempt_timeout_ms` | `ENGINE_ASSISTANT_SPEECH_TIMEOUT_MS` | `30000` | per-attempt bound on one speech upstream, not on the whole request |

The Vogt half of the assistant needs no key of its own: it uses
`vogt_core_url` — the same core the front door proxies — and the
`vogt_core_token_file` pairing on each front-door token's `extra_tokens`
entry (§3). A deployment with only the shared `vogt_core_token` gets Vogt
*reads* in the assistant and a named refusal on writes; pairing a token is
how it opts that token in.

#### Server-side speech (FR-T12, r16)

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
on-device recognition or typing (FR-T6). Audio is never stored.

Both lists **ship empty**: an earlier defaulted list made `/api/config`
advertise a backend nobody was running (r26). voicemode's own lists are the
paste-in when you want its shape — e.g. a local whisper server first, then a
hosted fallback:

```bash
ENGINE_ASSISTANT_STT_BASE_URLS=http://127.0.0.1:2022/v1,https://api.openai.com/v1
ENGINE_ASSISTANT_TTS_BASE_URLS=http://127.0.0.1:8880/v1,https://api.openai.com/v1
ENGINE_ASSISTANT_STT_API_KEY=sk-...   # used only by the entry that needs it
ENGINE_ASSISTANT_TTS_API_KEY=sk-...
```

An optional first-party alternative is the Rust `voice/` sidecar. Its native
providers are `whisper-rs` (GGML Whisper) and `piper-rs` over ONNX. They load
operator-mounted model files in-process; no executable or shell is involved,
and audio is not retained. The first-party Compose path is
`deploy/voice.firstparty.overlay.yml`; it requires an operator model directory
and keeps that mount read-only. A missing or invalid required model keeps the
sidecar unhealthy. If native model paths are absent, either half can instead
use the explicit JSON-argv subprocess adapter, or remain unconfigured. See
[`voice/README.md`](../voice/README.md) for model naming, supported
WAV/WebM/Opus/Ogg input and WAV output limits, placeholders, and the contract.
The existing
`deploy/voice.overlay.yml` third-party overlay is independent and unchanged.

#### Provider profiles (FR-T9, r16)

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
(FR-T7, evaluated per profile — the hang is one proxy's property, not the
deployment's). Approving a card resumes on the profile that proposed it: an
approval that continued on another model would hand one conversation's tool
results to a model that never saw it.

The route-level guard fires only when *no* configured profile can answer.
With one profile that is the original rule exactly; with two it is the honest
generalisation — a broken second profile must not stop the history of a
conversation held on a working one from being read.

`/api/config` advertises `assistant_profiles: [{name, model, default}]` and
**never a key or a base URL**: a browser offering the choice needs neither,
and a base URL is an exposure value (NFR-D2).

**A Claude subscription is not a profile.** It has no HTTP API to point a
`base_url` at, so the way to spend one is the `Claude Code (protected)`
session template — a session, not the assistant loop.

#### `claude-*` model ids are refused by default (FR-T7)

Validated in August 2026 against hosted OpenAI-compatible proxies: GPT models
responded quickly with correct tool calls, while the proxies' `claude-*`
routes hung. A hang is the worst failure a chat surface can have, being
indistinguishable from thinking, and the 60-second client timeout that used
to catch it reported "took too long" for something that was never going to
answer. So a `claude-*` model id on this transport is **refused rather than
avoided by convention**: every assistant route answers with a sentence naming
the model, the transport and the setting that overrides it.

`assistant_allow_claude_proxy` (per profile, or `ENGINE_ASSISTANT_ALLOW_CLAUDE_PROXY`
for the implicit default) turns the refusal off. It exists because the fault
is a *proxy's* rather than the model's: a deployment whose proxy serves those
routes correctly is entitled to say so and to own the result.

**The loop is OpenAI-compatible only, and that is a decision.** A native
Anthropic backend was once asked for as well and was deferred at r12, on the
grounds that the hang was the failure worth fixing and a second transport
buys a choice of vendor rather than a capability.

### Tools

The engine's own four are literals in `assistant.rs` — they are this
process's surface onto its own PTYs:

| Tool | Effect |
|---|---|
| `list_sessions` | id, name, command, activity state, exit code, cwd, created_at for every session |
| `read_session_tail` | last N bytes of a session's scrollback (default 4 KiB, max 16 KiB), ANSI-stripped by default |
| `send_input` | type text (max 4 KiB) into a session's PTY, optional Enter |
| `steer_agent_task` | queue a steer (`{task_id, text, interrupt?, reason?}`) for a task's in-flight run, delivered at its next prompt boundary (#289) |

`send_input` pauses for on-screen approval before it types (§6). `steer_agent_task`
does not: unlike `send_input`, which can inject arbitrary bytes into any session,
a steer reaches only a task's own in-flight run, is held until that run is at a
safe boundary, and is audited on the `task.steered` event with the actor
recorded as `assistant`.

#### The Vogt tools are fetched, not written (FR-T1)

Vogt's operation registry generates its own MCP tool schemas, and the core
serves them at `/mcp`. At the start of every turn the assistant POSTs
`tools/list` to the core and converts each `Tool` into an OpenAI function:
an MCP `inputSchema` is already JSON Schema, so it is forwarded **verbatim**
rather than restated here. A hand-written copy would be correct exactly once,
and would then drift silently as the registry changed.

- **Naming.** `work.get` → MCP `work_get` → function `vogt_work_get`. The
  `vogt_` prefix keeps the engine's `list_sessions` and Vogt's `session_list`
  from ever being confused, by the model or by the dispatcher.
- **Curation.** The slice is named in `vogt_tools.rs` and intersected with
  what the core actually serves. Reads: `backlog`, `bugs`, `why`,
  `project.brief`, `project.list`, `work.get`, `work.list`, `compliance`,
  `inbox.list`. Writes: `work.create`, `work.transition`, `work.comment`,
  `session.start`.
  **`inbox.list` and not `notifications`** (FR-T10, r16): "are there any
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

#### Which credential a Vogt call uses (FR-T3)

The assistant runs server-side but is always *called* by an authenticated
user. `require_bearer` leaves an `AuthorizedIdentity` in the request
extensions carrying the front-door token's name and the vogt-core token paired
with it (FR-S9); `assistant_api.rs` turns that into a `Caller` and hands it to
the runtime. There is no other credential in reach of the tool loop.

| | Credential | If the caller has no pairing |
|---|---|---|
| Read | the caller's pairing, else the deployment-wide `vogt_core_token` | falls back, because a read attributes nothing |
| Write | the **approving** caller's pairing, and nothing else | refused, with the token's name and what to configure |

The asymmetry is the whole of FR-T3. A write filed under a shared token names
the wrong actor in an audit row somebody reads months later, and a wrong answer
there is worse than a refusal a user can act on. The credential is taken from
the request that *approved* the action, not from the one that sent the message
that proposed it — when those differ, FR-T3 is about the second.

Front-door capabilities gate reaching the assistant at all (`assistant` on
every mutating assistant route). What a given write is *allowed* to do in Vogt
is enforced at the core against the approver's own core token (FR-S4), which
is the same check any other client of the core gets.

### Threat model

Extends the rule §7 states for agent tasks: external content must never
become instructions.

- **Terminal output is untrusted.** Sessions run arbitrary programs, including
  other AI agents consuming untrusted web content. Anything those programs
  print can reach the assistant's context via `read_session_tail`. Tool
  results are wrapped in `<terminal-output>` delimiters and the system prompt
  tells the model to treat embedded instructions as data — but that is
  defense-in-depth, not the guarantee.
- **Vogt content is untrusted too, by the same rule (FR-T4).** A work item's
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
- **Vogt writes use the same gate, with no auto-type escape (FR-T2).** A
  mutating `vogt_*` tool is intercepted in the same place and pauses the same
  loop; the pending action carries the operation, a one-line target, the
  exact arguments pretty-printed, and the `reason` that will be written to
  Vogt's audit log. Nothing reaches the core until the action is approved.
  Typing into a terminal and
  deliberately does not extend to Vogt: there is no configuration that lets a
  model's output become a Vogt write unattended. A write proposed without a
  reason is refused before it becomes a card at all — a card that cannot say
  what will be recorded is not an approval anyone can meaningfully give — and
  the dispatcher refuses a mutating Vogt tool outright if it ever reaches it,
  so a future edit that loses the interception fails closed instead of
  writing.
- **No other effectors.** The assistant has no file, network, git, or config
  tools, and its Vogt reach is the curated slice — no `token.issue`, no
  `restore`, no `import`, whatever else the core may serve. Its blast radius
  is: read scrollback, read that slice of Vogt, and (after approval) type into
  a PTY or make one of four Vogt writes as the approving user.
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
- **Token scoping:** mutating assistant routes require the `assistant`
  capability. A token holding it transitively gains type-into-any-session
  that approval and should only be set where the token boundary is already
  trusted. It does **not** transitively gain Vogt write power: that needs a
  paired core token whose own scopes the core enforces.

One clause of the assistant's requirements is short, and it is not described
above as though it existed: FR-T5's spoken validation pass of the recogniser
against domain vocabulary (project slugs, `WI-n` ids) is a device-dependent
pass run per release rather than in CI ([`ROADMAP.md`](ROADMAP.md)). FR-T7's
native Anthropic backend was the other, and was deferred rather than left
owed — the provider section above.

### Voice

- **STT** — `@capacitor-community/speech-recognition` (on-device Android
  recognizer), only inside the APK; browsers fall back to typing. Push-to-talk
  via the mic button — **held**, not tapped: press to open the microphone,
  release to send. Partial results land in the composer and the release sends
  what the recognizer settled on. Held rather than toggled because the take
  auto-sends: a toggle left on in a room with other people does not merely
  listen, it eventually speaks. It is also holdable from the keyboard with
  space or enter, since a control only pointers can work is one some people
  cannot use. This paragraph said "push-to-talk" for two revisions while the
  button was a toggle; now it is one. `RECORD_AUDIO` is declared in the
  manifest; the plugin prompts at first use.
- **TTS** — Web Speech `speechSynthesis`, sentence-chunked, toggle persisted
  in localStorage. The synth is primed on the toggle gesture because the
  Android WebView requires a user gesture before the first utterance.

### `assistant_auto_type` is gone *(r9)*

The setting removed the approval gate for `send_input` — the one tool
FR-T2 names first — and defaulted off, which made it look harmless. It was
not: r9 promoted this gate to a numbered requirement *on the stated grounds
that it is "a structural guarantee, not configuration"*, and a switch that
turns it off is configuration by definition. The requirement and the setting
could not both be true, and the requirement is the one this product argues
for.

What is lost is a convenience for a trusted single-user setup: an assistant
that types without asking. What is kept is the sentence the threat model
opens with — no model output reaches an effector without an on-screen act —
being true of every tool, in every deployment, rather than of most tools in
most deployments.

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
- Sessions created with a `prompt` (FR-E4's work-item brief) share that
  mechanism: their brief is written to
  `state_dir/agent-task-prompts/sessions/<session-id>.md` and exported as the
  same `MYDEVENV2_AGENT_TASK_PROMPT_FILE` variable, so one prompt root exists
  and `POST /api/agent-tasks/artifacts/cleanup` accounts for everything under
  it. See `engine/server/src/prompt_files.rs`.
- Tasks can schedule `manual`, `interval`, or UTC `daily` runs. The first useful
  product-monitor shape is `interval { minutes = 720 }` for twice daily.
- The default notification hook is output-driven: if an agent prints a line
  beginning with `MYDEVENV2_NOTIFY:`, the server fans out a push notification
  linking back to that run's session.
- The same line is now **recorded on the run** as a finding — `{at, text,
  source}` on `AgentTaskRun.findings` — before the push is sent. A push is a
  delivery, not a record: if nothing was subscribed, or the phone was off, or
  the push service was down, what the agent found used to be gone. The push is
  unchanged; this is the durable copy beside it.
- A task may name a **Vogt subject** it is about: `vogt_project` (a slug) or
  `vogt_work_item` (a ref like `WI-7`), or both. The engine does not resolve
  either — it exports them into the run as `VOGT_PROJECT` / `VOGT_WORK_ITEM`
  and names the subject in the run's prompt file, so the agent knows what it
  is reporting on. vogt-core's `session-outcomes` collector reads the bound
  tasks and files their runs' findings as observations against that subject
  (Vogt FR-E7). Nothing is pushed from here into Vogt's stores: the binding
  makes the run *collectable*, and collection stays Vogt's own act.

Example task payload for the Hisense PX3 monitor:

```json
{
  "name": "Hisense PX3 price monitor",
  "prompt": "In Australia, check current online prices for the Hisense PX3. Compare against prior context. If the best price is lower than the previous best, print exactly: MYDEVENV2_NOTIFY: Hisense PX3 dropped to <price> at <store>. Otherwise summarize quietly.",
  "schedule": { "kind": "interval", "minutes": 720 },
  "command": ["codex", "exec", "--prompt-file", "{prompt_file}"],
  "context": "Track best observed price, store, URL, and timestamp here."
}
```

The command is intentionally user-supplied. The engine does not depend on a
specific AI CLI, because production deliberately leaves Codex, Claude and
other agents user-managed.

#### External workflow providers

For a task whose agent loop is owned by a workflow service, add
`workflow_engine` with `engine_url`, a non-empty `workflow` label, and the
optional `token_file`/`repo_ref` fields. The original `/api/runs` REST + SSE
contract remains available for compatible services.

Fabro is supported through the explicit `workflow_engine.fabro` object. It
uses the immutable workflow-intent API when `workflow_version_id` is supplied,
and can use the self-contained manifest lane with `workflow_source` for Fabro
0.254 servers. It never derives a workflow identity from a repository ref:

```json
{
  "engine_url": "https://fabro.example",
  "workflow": "nightly audit",
  "token_file": "/run/secrets/fabro-token",
  "fabro": {
    "workflow_version_id": "<64 hexadecimal characters>",
    "target": {"kind": "git", "repo": "acme/project", "branch": "main"},
    "environment_id": "default",
    "model": "<optional model>",
    "provider": "<optional provider>"
  }
}
```

For Fabro 0.254, use the compatibility form instead:

```json
"fabro": {
  "workflow_source": "digraph Smoke { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }",
  "workflow_path": ".fabro/workflows/smoke/workflow.fabro",
  "target": {"kind": "folder", "path": "/srv/workspaces/project"},
  "environment_id": "local"
}
```

The engine creates a run with `POST /api/v1/runs`, explicitly starts it with
`POST /api/v1/runs/:id/start`, polls `GET /api/v1/runs/:id`, and follows the
ordered `GET /api/v1/runs/:id/attach?since_seq=1` SSE stream. Pending
questions are read from `/questions` and answered using Fabro's keyed answer
forms; steering is sent to `/steer`. Git targets require `repo` and
`branch`; folder targets require an absolute existing server-side directory;
`none` is an empty target. Git-target polling reads Fabro’s durable timeline
and records its stage entries against the provider run branch. Fabro folder
runs execute in place and therefore do not provide git checkpoint branches;
Vogt reports no fabricated checkpoint for them. Canonical billing, diff
summary, final commit and terminal status are retained when Fabro returns them.
A failed or unreachable optional
provider records the workflow run as errored and does not break the engine.
The manifest path was live-smoked against Fabro 0.254 on 2026-08-26. The
immutable workflow-intent Git path was also live-smoked against Fabro
0.337.0-nightly.1 on 2026-08-26, targeting Vogt's public `dev` branch: version
creation, Git clone, run start, durable `fabro/run/<run-id>` and `start@1`
timeline checkpoint projection, attach SSE replay, and successful terminal
diff projection all completed. That smoke used no model, so Fabro correctly
returned no charge; the adapter retains `total_usd_micros` when supplied, as
covered by its wire-shape tests. The repository also carries an ignored Rust
test for repeating the manifest smoke with `FABRO_LIVE_URL`,
`FABRO_LIVE_TOKEN`, `FABRO_LIVE_FOLDER`, and `FABRO_LIVE_WORKFLOW_SOURCE`.

### Approval gates and mid-run steering (#289)

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

What a task's findings are *for* is FR-E7 — a bound run's
findings are collectable by vogt-core's `session-outcomes` collector, as
evidence with freshness and trust like everything else. Nothing is pushed from
the engine into Vogt's stores; the binding makes the run collectable, and
collection stays Vogt's own act.

---

## 8. What the engine does not do

Named here so the absences read as decisions rather than omissions.

- **It never decides to run anything.** Every session traces to a person or to
  a schedule a person created. Autonomous work pickup is deferred by name,
  and it is the surviving core of the non-goal r9 reversed.
- **It is not an IDE.** Monaco is a tab type; there are no language servers in
  the client, no extension ecosystem, and no collaborative editing.
- **It has no editor logic, terminal rendering, or language servers
  server-side.** Those are client concerns, and keeping them there is what
  stopped v1's code-server fork happening again.
- **The assistant has no file, network, git or config tools.** Its blast radius
  is: read scrollback, read a curated slice of Vogt, and — after an on-screen
  approval — type into a PTY or make one of four Vogt writes as the approving
  user.
- **There is no native desktop client.** A reference to `client/`
  anywhere in this tree names something that is not here.
- **Nothing here is a second backlog.** The engine's outstanding work lives in
  the GitHub issue tracker and `ROADMAP.md`, because an item without a
  requirement is nobody's plan.

## 9. Optional integrations — what each does when absent

The engine ships client code for a number of external services. Every one of
them is **absent by default**, and its absence is a reported, honest state
rather than a fault (NFR-O5) — an engine with all of them off is a complete
product. This table is so a reader is also told what each one costs when the
service behind it is not there, and exactly which setting turns it on.

| Integration | What it is for | Turned on by | What its absence means |
|---|---|---|---|
| **Assistant provider** (any OpenAI-compatible chat endpoint) | the assistant's tool-use loop | `ENGINE_ASSISTANT_API_KEY` + `ENGINE_ASSISTANT_BASE_URL` (+ `_MODEL`, profiles) — §6 | The assistant routes answer 404 and the PWA hides its tab (FR-T6). A key with no `_BASE_URL` is a *startup error*, not a silent default (r20). |
| **Speech provider** (OpenAI-compatible audio endpoints) | server-side STT/TTS for the assistant | `ENGINE_ASSISTANT_STT_BASE_URLS` / `ENGINE_ASSISTANT_TTS_BASE_URLS` (+ `_MODEL`, `_VOICE`, `_API_KEY`) — §6 | Each half answers 404 independently; the client falls back to on-device speech or typing. |
| **Vogt core** | the front door, the assistant's Vogt tools, the event follower | `VOGT_CORE_URL` + `VOGT_CORE_TOKEN` / `VOGT_CORE_TOKEN_FILE` — §3, §5 | The engine is bootable alone (FR-E9): sessions work, `/readyz` stays ready, the Vogt routes answer `503` with a named reason. |
| **FCM** (native push) | push to the Android shell | `ENGINE_FCM_SERVICE_ACCOUNT_JSON` (a Firebase service-account JSON, single line) | The FCM transport is disabled; browser web-push still works for any subscription. VAPID keys are generated and persisted under `state_dir`. |
| **GUI streaming** | the GUI tab's live stream of launched processes | `GUI_STREAM_URL` (+ `START_SWAY=1`, and `GUI_STREAM_VERIFIED=1` once an operator has watched it work) | `/readyz` reports `gui: disabled` and the GUI surface's affordances are withdrawn with a stated reason (FR-E10). |
| **Agent CLIs** (`codex`, `claude`) | agents inside sessions | `INSTALL_AI_CLIENTS=true` at image build, or a user-managed install in the pod's home | Sessions are ordinary shells; the two "(protected)" templates cannot start. |
| **Cadastre** (external MCP server) | an extra MCP server for agents in a session | `INSTALL_CADASTRE_MCP=true` at build + `CADASTRE_MCP_ENABLED=1` + `CADASTRE_MCP_URL` (§4) | Agents in a session cannot reach it; every other MCP server and all core function is unchanged. A separate product, never assumed present. |
| **Agent service auth** | brokering third-party service credentials (GitHub and others) into a session from a secrets manager | `ENGINE_AUTO_AGENT_AUTH=1` + `ENGINE_AGENT_AUTH_HELPER` naming a helper (the bundled Infisical example is auto-selected from a machine identity), configured through the env vars in `agent-auth.sh`'s header | Sessions run without those credentials pre-loaded; nothing in the engine depends on it. **The shipped helper is one pluggable example** — it bakes in no address, project or secret name and is driven by `ENGINE_AGENT_AUTH_SECRETS`/`_PROBES`; point the variable at your own, or leave it off. |

Operator-local notes about a particular deployment belong in the git-ignored
`docs/local/`, not here.

The core product's own optional integrations — GitHub collection, MCP, remote
MCP, and the session engine itself — are in
[`CUSTOMISATION.md`](CUSTOMISATION.md), which points here for the engine's.
