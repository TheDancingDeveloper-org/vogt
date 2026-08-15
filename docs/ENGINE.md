# Vogt — The Session Engine

Status: **built and current as of 2026-08-15** · this document describes what
the engine *is*, not what it was planned to be. Where a capability was designed
and never delivered, it is not described here — it is a numbered gap in
[`REQUIREMENTS.md`](REQUIREMENTS.md) §7.

The session engine is the Rust half of Vogt. It was MyDevEnv2, a standalone
product, until the merge recorded in [`MERGE_MYDEVENV2.md`](MERGE_MYDEVENV2.md)
made it Vogt's execution surface. It runs PTYs, streams them over WebSocket,
serves the PWA, and — since M9 — is the merged product's **front door**: the
only listening process, proxying `/api/vogt` and `/mcp` to the Python core on
loopback.

This file is the single reference for the engine. It replaced eight separate
documents (`API_CONTRACT.md`, `ASSISTANT.md`, `AGENT_TASKS.md`, `INTENT.md`,
`PLAN.md`, `TOOLING.md`, `USER_GUIDE.md`, `uplift.md`) that described MyDevEnv2
as its own product, each with its own idea of what the product was called and
which tree it lived in. Their still-true content is here, in
[`DEPLOYMENT.md`](DEPLOYMENT.md) §10–§11, in
[`USER_GUIDE.md`](USER_GUIDE.md), and — for everything they proposed and
nobody built — in `REQUIREMENTS.md` §7.

Companion documents: [`DESIGN.md`](DESIGN.md) (the product's architecture),
[`REQUIREMENTS.md`](REQUIREMENTS.md) (the numbered baseline, including every
requirement the engine carries), [`DEPLOYMENT.md`](DEPLOYMENT.md) (topologies,
the runtime image's toolchain, and the stacks), [`USER_GUIDE.md`](USER_GUIDE.md)
(how a person drives it).

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
  UTC `daily` — whose runs are real PTY sessions, optionally bound to a Vogt
  project or work item (FR-E7).
- **The assistant.** A server-side tool-use loop over sessions and a curated
  read slice of Vogt, with every effector behind an on-screen approval
  (FR-T1–T4, FR-T6).
- **Workspace-scoped file and git APIs**, a GUI process launcher, web push
  (VAPID and FCM), archived session history, and a ContextKeeper continuity
  proxy.
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
  server/         mydevenv2-server: PTYs, HTTP, WS, SSE, the front door
  contract/       mydevenv2-contract: shared wire DTOs
  Dockerfile      the merged image (context is the repository root)
  deploy/         entrypoint, agent-auth helpers, the standalone compose
  .woodpecker/    the engine's own CI, publishing to the Forgejo registry
web/              the Solid/Vite PWA — the product's GUI, embedded at build time
mobile/           the Capacitor 8 Android shell that loads the deployed PWA
```

Crate names, the config prefix (`MYDEVENV2_*`) and the container's own
identity still say MyDevEnv2. That is deliberate and is not drift: the names
divide by *process*, not by product, and renaming them is a stack-environment
migration on a live deployment. `MERGE_MYDEVENV2.md` §11.1 records the sunset
order — move the host, retire the standalone stack, *then* alias the names.

`engine/` is its own Cargo workspace, so every `cargo` invocation runs from
`engine/`. The Rust binary embeds the repository-root `web/dist/` via
`rust-embed` at compile time, which means **a `cargo build` without a fresh
`pnpm build` ships a stale frontend** — the most common way to fix a UI bug and
see nothing change.

## 3. Running it

```bash
# 1. Mint a token (>=16 chars)
export MYDEVENV2_TOKEN="$(openssl rand -hex 24)"

# Optional: scoped tokens and a write-rate cap for the primary token.
# Capability names: sessions, filesystem-write, git-write, gui-control,
# agent-tasks-write, push-write, history-write, assistant, vogt-write
export MYDEVENV2_MUTATING_REQUEST_LIMIT_PER_MINUTE=600
export MYDEVENV2_EXTRA_TOKENS_JSON='[
  {"name": "readonly", "token": "replace-with-another-16+-char-secret",
   "capabilities": []}
]'

# 2. Run — from engine/, which is the Cargo workspace root
cd engine
cargo run -p mydevenv2-server -- --bind 127.0.0.1:8910
```

Optional TOML config, passed with `--config mydevenv2.toml`. Precedence is
CLI flags > env > config file:

```toml
bind = "0.0.0.0:8910"
token = "..."                  # or MYDEVENV2_TOKEN
scrollback_bytes = 4194304
default_shell = "/bin/bash"
default_cwd   = "/home/sprooty/Working"
workspace_root = "/home/sprooty/Working"
activity_idle_after_ms = 1500
state_dir = "/home/sprooty/.local/share/mydevenv2"
vapid_subject = "mailto:admin@example.invalid"
allowed_origins = [
  "https://vogt.sprooty.com",
  "https://mydevenv2.sprooty.com",
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
policy: keep `MYDEVENV2_TOKEN` as the admin/recovery credential, use a scoped
interactive token for normal browser use, a scoped read-only token for passive
viewers, and a separate `gui-control` token if a browser regularly launches GUI
processes. The PWA's Settings modal stores device-local named auth profiles, so
the primary token need never sit in a browser's `localStorage`.

Which values the *core* reads is [`CONFIG.md`](CONFIG.md), which is generated
from `src/vogt/config.py` and does not describe this process.

### 3.1 Refreshing the embedded PWA

```bash
cd web && pnpm install && pnpm build
cd ../engine && cargo build --release
```

For UI work, run the server and the Vite dev server in parallel — Vite proxies
`/api` and the WebSocket endpoint to the backend:

```bash
# terminal 1
cd engine
MYDEVENV2_TOKEN=$(openssl rand -hex 24) cargo run -p mydevenv2-server -- --bind 127.0.0.1:8910
# terminal 2
cd web && pnpm dev   # -> http://127.0.0.1:5173, paste the token into Settings
```

### 3.2 Tests

```bash
cd engine                        # the Cargo workspace root
cargo fmt --check
cargo clippy -- -D warnings
cargo test                       # server unit + integration (HTTP + WS)
cargo test -p mydevenv2-contract # shared wire-contract tests

cd ../web && pnpm typecheck      # PWA TypeScript check
cd ../web && pnpm test           # 75 jsdom tests over the five Vogt surfaces
```

The Python core's suite runs from the repository root and does not need either
toolchain — `NFR-Q6` keeps it that way, and CI runs it with `engine/`, `web/`
and `mobile/` deleted to prove it.

### 3.3 Smoke test with curl + websocat

```bash
TOKEN=$MYDEVENV2_TOKEN
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

The merged stack has its own five-check smoke script,
`scripts/smoke_merged_stack.sh` — see `DEPLOYMENT.md` §9.2. It exists because
the failure worth catching is not a crash but a front door that comes up,
passes its healthcheck and serves no Vogt.

## 4. Agent-facing MCP servers inside the pod

The runtime image bundles two MCP servers so agents running in a session can
reach a Rust LSP and GitHub without the user wiring anything.

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
build time. It needs `GITHUB_PERSONAL_ACCESS_TOKEN`; inside an
`mydevenv2-agent-auth` shell that is already available as `$GH_TOKEN`.

```bash
codex mcp add github -- env GITHUB_PERSONAL_ACCESS_TOKEN=$GH_TOKEN github-mcp-server stdio
claude mcp add --scope project github -e GITHUB_PERSONAL_ACCESS_TOKEN=$GH_TOKEN -- github-mcp-server stdio
```

Use `GITHUB_AUSAGENTSMITH_PAT` instead of `GH_TOKEN` for a second instance
scoped to the AusAgentSmith-org identity.

**Vogt.** A session started for a project or work item registers Vogt's own MCP
server automatically, carrying a per-session actor-scoped token, so an agent's
writes are attributed to that session's actor rather than to a shared identity
(FR-E5, FR-S10). `DEPLOYMENT.md` §7.2 is the credential's story.

---

## 5. The wire contract

This section is the source-of-truth summary for the engine's wire contract.

`engine/server/src/app.rs` holds the route table and is what actually answers
requests; this section describes it route by route. `tests/test_pwa.py` resolves
every engine path in the shipped PWA against both, so the two cannot drift
apart silently (FR-U8). When they do disagree, this section is the one that
is wrong.

### Contract crate

Rust DTOs live in `engine/contract/` (`mydevenv2-contract`). They were shared
with the archived native client, which the merge left behind, so the server is
now the only consumer in this tree. Those types cover:

- session lifecycle payloads
- SSE event payloads
- file and git API payloads
- WebSocket attach control frames
- common small response shapes like `{"ok": true}`

The browser client still carries TypeScript mirrors in `web/src/api.ts`, but
those shapes should follow the shared Rust contract instead of ad hoc
server-local structs. The browser/PWA is the supported client surface; the old
native desktop client remains deprecated legacy code.

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
  `gui-control`, `agent-tasks-write`, `push-write`, `history-write`,
  `assistant` and `vogt-write`; the primary token holds all nine, and a scoped
  token holds what its `extra_tokens` entry lists. The mapping lives in
  `required_capability` in `engine/server/src/auth.rs` and is keyed on method
  *and* path, so `GET /api/sessions` needs no capability while
  `POST /api/sessions` needs `sessions`.
- A token that authenticates but lacks the capability gets `403`, not `401`.
  The distinction is worth acting on: `401` means try a different credential,
  `403` means this credential will never work for this route.
- Every mutating request (POST/PUT/PATCH/DELETE) is rate limited per token —
  600 per minute by default, `mutating_requests_per_minute` per scoped token.
  Over the limit is `429` with `Retry-After` in whole seconds.
- Every response from a gated route carries `X-Request-Id`, echoed from the
  request when it sent one and minted otherwise. Mutating requests are written
  to the audit log under the same id, with the token's *name* and never its
  value.
- Errors are `{"error": "<message>"}` with the status on the HTTP line: `400`
  malformed or out-of-bounds input, `401` no or wrong token, `403` capability
  denied, `404` not found (also: feature not provisioned, see below), `409`
  conflict, `429` rate limited, `500` something the engine owns is broken,
  `502` an optional upstream did not answer. When an upstream answered and its
  own reply is the useful one, its status is passed through and the body is
  `{"error": ..., "detail": <upstream body>}`.
- A feature that is not provisioned answers `404` rather than `501` or `503`:
  the assistant with no API key, every continuity route but its health probe
  with no ContextKeeper. The feature is invisible rather than
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
  (skipped unless `TAILSCALE_AUTH_KEY` is set), `gui` (skipped unless
  `START_SWAY`), `vogt_core`, `workspace_agreement` (the core imports inside
  this server's workspace root — FR-E3) and `backup_agreement` (`vogt backup`
  would cover this server's `state_dir` — NFR-I6). `200` when every *fatal*
  check passes, `503` otherwise; the last three are non-fatal by design, so a
  ready container can still be reporting one of them false.
- `GET /api/config` -> `PublicConfig`
  `{gui_stream_url, version, features, session_templates, assistant_enabled,
  assistant_model?}` — what the browser needs at boot, before the user has
  typed a token into Settings. It is outside the gate because it returns no
  secrets: `assistant_enabled` is presence only, never the key.
  `features` is read per request from `/etc/mydevenv2/features.json` and is
  `{}` when that file is absent.
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
the session was created with; absent for default-shell sessions.

`SessionSpec` (the `POST /api/sessions` body) carries an optional `prompt`
field — the brief the session's agent should start from:

```json
{
  "name": "VOGT-42 fix the flaky forge test",
  "cwd": "Active/apps/vogt",
  "prompt": "Fix the flaky forge test.\n\nWhy: it blocks the release."
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
    "cwd": "Active/apps/MyDevEnv2",
    "created_at": "2026-07-06T00:00:00Z"
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

`SessionSummary.continuity` is ContextKeeper enrichment and is **absent**, not
null, when there is nothing to say — no sidecar configured, sidecar
unreachable, or no agent session bound to the PTY. It is read from a cache the
ContextKeeper runtime refreshes in the background, so a slow sidecar costs a
stale badge rather than a hung roster. `POST /api/sessions` never enriches: a
brand-new terminal has nothing bound yet, and creating one must not wait on
anything.

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
  `session-killed` and `activity`, each tagged by `type`.
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

All routes 404 unless the server has `MYDEVENV2_ASSISTANT_API_KEY`
provisioned. Mutating routes require the `assistant` token capability. See
§6 for the threat model and behavior.

- `POST /api/assistant/message` `{"text": "..."}` ->
  `{"reply": string|null, "pending_action"?: PendingAction, "tool_trace"?: string[]}`
- `POST /api/assistant/actions/:id` `{"approve": bool}` -> same reply shape
- `GET /api/assistant/history` -> `{"transcript": [...], "pending_action"?: ...}`
- `POST /api/assistant/reset` -> `OkResponse`

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

A transcript entry is `{"role", "text", "tool_trace"?}`. `reply` is null when
the turn paused on a pending action before the model produced any text, which
is the state a client should render as "waiting for you", not as an empty
answer.

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
  arrives later, on the task's `runs`.
- `POST /api/agent-tasks/artifacts/cleanup`
  `{"keep_latest_runs_per_task": 10}` -> `PromptArtifactCleanup`
  `{removed_task_dir_count, removed_prompt_file_count,
  removed_context_file_count, removed_session_prompt_file_count,
  removed_bytes}`

`AgentTask.schedule` is tagged by `kind`: `manual`, `interval` with `minutes`,
or `daily` with `times`. `status` is `active` or `paused`. A run carries
`status` (`running`, `completed`, `errored`), the session it spawned, and the
paths of the prompt and context files written for it.

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
of a phone that can replay an estate's history after a restart; a missed buzz
is recoverable by opening the app, and a notification channel someone switched
off is not.

Notifications the engine sends carry `{kind, session_id, url}` in their data,
where `url` is a PWA route (`/#/t/<session-id>`), so a tap lands on the
terminal that raised it.

### Continuity APIs

A same-origin allow-list in front of the ContextKeeper sidecar. The browser
never receives ContextKeeper's control token, so every call is forwarded
server-side — and only these calls are, because ContextKeeper's own API also
carries prune and maintenance routes no browser should reach. With no
ContextKeeper configured, every route here except `health` answers `404`, which
the PWA reads as "continuity is unavailable" rather than as an error.

- `GET /api/contextkeeper/health` -> a snapshot of whether continuity is
  configured and reachable, and how fresh capture is. `200` even when the
  sidecar is unreachable — `{"configured": false, "reachable": false}` is the
  answer that lets the PWA show terminals as unprotected instead of pretending
  the feature does not exist.
- `GET /api/contextkeeper/terminals/:id` -> `SessionContinuity` for one PTY,
  keyed by MyDevEnv2's session id, or `{"state": "unprotected"}` when nothing
  is bound to it.
- `GET /api/contextkeeper/sessions/:session_id` -> the registry session.
- `GET /api/contextkeeper/sessions/:session_id/continuation` -> the
  continuation recipe. ContextKeeper picks the rung; MyDevEnv2 creates the PTY
  from the recipe's command, cwd and env. `kind: "reattach"` means attach the
  existing terminal and start nothing.
- `GET /api/contextkeeper/sessions/:session_id/preview` -> the compiled
  recovery bundle.
- `POST /api/contextkeeper/sessions/:session_id/approve`
  `{"bundle_id", "request_id"?}` -> the approval record.
- `POST /api/contextkeeper/sessions/:session_id/launch`
  `{"bundle_id", "request_id"?}` -> the launch result.
- `GET /api/contextkeeper/work/:work_id` -> every attempt in one durable work
  session, so earlier attempts stay reachable after a recovery has replaced the
  terminal.

The two POST routes here are the only writes in this file that require no
capability — a valid token of any scope may approve a bundle and launch a
recovery, which spawns a terminal. Stated rather than left to be discovered:
a client cannot tell from a `403` it will never get.

Preview is a separate step from approval on purpose: approval is a human
deciding about *this* bundle, so the UI must have shown it first. `request_id`
is supplied by the client so that a retried click replays the same operation
instead of performing a second one; when omitted the engine mints one, which
makes the retry protection the caller's to keep.

Bodies here are ContextKeeper's shapes, passed through as JSON rather than
re-declared in `engine/contract/`. That is deliberate — they belong to the
sidecar's contract, and mirroring them would create a second place to be wrong.

### Session history APIs

Archived scrollback for sessions that have ended. Every route requires history
to be enabled — it is disabled when the store fails to open, and then these
routes answer `500`, not `404`, which is the one place in this file where an
absent feature does not read as absent.

- `GET /api/history/sessions?limit=&offset=` -> `SessionMetadata[]`; `limit`
  defaults to 50.
- `GET /api/history/search?q=&limit=` -> `SearchResult[]` — full-text over
  archived output, ranked; `limit` defaults to 20.
- `GET /api/history/:id` -> `SessionMetadata`
- `GET /api/history/:id/log?tail_bytes=` -> `SessionLogPreview`
  `{session_id, text, bytes, total_bytes, truncated}` — the *tail*, 64 KiB by
  default. `truncated` is how a client knows it is not looking at the whole
  run.
- `GET /api/history/:id/download` -> the whole log, streamed, as an attachment
  named for the session.
- `DELETE /api/history/:id` -> `{"ok": true}`, `404` if it was already gone
  (requires `history-write`).
- `POST /api/history/cleanup` `{"retention_days": 30}` ->
  `{"ok": true, "removed_sessions", "retention_days"}` (requires
  `history-write`).

### The Vogt front door

Three route families proxy to vogt-core, the Python half of the merged product.
It runs beside the engine on loopback and is never published, so everything a
client asks of it arrives here first (NFR-D11). `engine/server/src/vogt_core.rs`
is the implementation and argues the decisions.

| Front door | vogt-core | Auth |
|---|---|---|
| `/api/vogt`, `/api/vogt/*` | `/api/*` | front-door token, core token injected |
| `/mcp`, `/mcp/*` | `/mcp` | the client's own core token, forwarded untouched |
| `/ui-legacy`, `/ui-legacy/*` | `/ui/*` | none — static assets, as at the core |

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

**`/mcp` — any method.** Deliberately outside the bearer gate. The credential
on an MCP request is already a *core* token, minted by `vogt token issue` and
bound to an actor, and it is forwarded untouched. Re-checking it against the
engine's unrelated token list would refuse every legitimate agent, and
rewriting it would replace a real actor with a shared one and make the core's
audit log worse. Responses stream: `/mcp` is streamable HTTP and its replies
are long-lived SSE, so there is no overall request timeout on the hop — only a
two-second connect timeout, which is what protects against a core that is not
listening.

**`/ui-legacy/*` — GET.** The vanilla GUI, served from here so that "the merged
product publishes one port" has no exception written into it (FR-U9). No token:
static files need none at the core either, and there has to be a page on which
to enter one. `/ui-legacy` permanently redirects to `/ui-legacy/` — not
cosmetic, because `index.html` links its stylesheet and module relatively and a
browser resolves those against the directory of the current document.

With no core configured, every route in all three families answers `503`; a
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
merged last so `/healthz`, `/api/*`, `/mcp` and `/ui-legacy` keep priority — a
new route added *after* it would be shadowed by it and answer with the
application shell, which looks like a client-side routing bug rather than a
server one.

### Response conventions

Avoid anonymous `serde_json::json!` blobs for stable routes when a named typed
response will do. Current standard small shapes:

- `OkResponse` -> `{"ok": true|false}`
- `WriteFileResponse` -> `{"ok": true, "bytes": <n>}`

Several routes here still return `serde_json::Value` — the push routes, the
continuity proxy, `DELETE /api/history/:id`, `DELETE /api/agent-tasks/:id`.
The continuity ones are that way on purpose, because the shapes are
ContextKeeper's. The rest are the standard above not yet applied.

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
- **ContextKeeper's own API.** Only the allow-listed subset above is proxied;
  the sidecar's contract is the sidecar's.

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
  gate.
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

### Configuration

| Key (TOML) | Env | Default |
|---|---|---|
| `assistant_api_key` | `MYDEVENV2_ASSISTANT_API_KEY` | unset (feature off) |
| `assistant_base_url` | `MYDEVENV2_ASSISTANT_BASE_URL` | `https://api.theclawbay.com/v1` |
| `assistant_model` | `MYDEVENV2_ASSISTANT_MODEL` | `gpt-5.4-mini` |
| `assistant_max_tool_calls` | `MYDEVENV2_ASSISTANT_MAX_TOOL_CALLS` | `8` |
| `assistant_reasoning_effort` | `MYDEVENV2_ASSISTANT_REASONING_EFFORT` | unset |
| `assistant_allow_claude_proxy` | `MYDEVENV2_ASSISTANT_ALLOW_CLAUDE_PROXY` | `false` |

The Vogt half needs no assistant-specific key. It uses `vogt_core_url` — the
same core the front door proxies — and the `vogt_core_token_file` pairing on
each front-door token's `extra_tokens` entry. A deployment with only the
shared `vogt_core_token` gets Vogt *reads* in the assistant and a named
refusal on writes; pairing a token is how it opts that token in.

The backend must be OpenAI-compatible (`POST {base_url}/chat/completions`
with `tools` / `tool_calls`). Notes from validation against The Claw Bay
(August 2026): GPT models respond quickly with correct tool calls; the
`claude-*` proxy routes hung and are **now refused rather than avoided by
convention** (FR-T7). A `claude-*` model id on this transport makes every
assistant route answer with a sentence naming the model, the transport and
the setting that overrides it — because a hang is the worst failure a chat
surface can have, being indistinguishable from thinking, and the 60-second
client timeout that used to catch it reported "took too long" for something
that was never going to answer.

`assistant_allow_claude_proxy` turns the refusal off. It exists because the
fault is a *proxy's* rather than the model's: a deployment whose proxy serves
those routes correctly is entitled to say so and to own the result. The check
is about the transport, so FR-T7's other clause — a native Anthropic backend,
still unbuilt — would not be subject to it.

### Tools

The engine's own three are literals in `assistant.rs` — they are this
process's surface onto its own PTYs:

| Tool | Effect |
|---|---|
| `list_sessions` | id, name, command, activity state, exit code, cwd, created_at for every session |
| `read_session_tail` | last N bytes of a session's scrollback (default 4 KiB, max 16 KiB), ANSI-stripped by default |
| `send_input` | type text (max 4 KiB) into a session's PTY, optional Enter |

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
  `project.brief`, `project.list`, `work.get`, `work.list`, `compliance`.
  Writes: `work.create`, `work.transition`, `work.comment`, `session.start`.
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

Two clauses of the assistant's requirements are short, and neither is
described above as though it existed: FR-T5's voice validation pass and
FR-T7's native Anthropic backend. `REQUIREMENTS.md` §7 carries both, with what
is missing and what it costs.

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

### External content is not instruction

A task's output is external content, and so is anything the agent inside it
read to produce that output — web results, issue bodies, notes, another
agent's transcript. None of it may become an instruction to this system. The
assistant enforces the same rule structurally at §6; here it is a property of
what a finding *is*: a recorded observation with a source, never a command.

What a task's findings are *for* is `REQUIREMENTS.md` FR-E7 — a bound run's
findings are collectable by vogt-core's `session-outcomes` collector, as
evidence with freshness and trust like everything else. Nothing is pushed from
the engine into Vogt's stores; the binding makes the run collectable, and
collection stays Vogt's own act.

---

## 8. What the engine does not do

Named here so the absences read as decisions rather than omissions. Each one
that is *owed* carries a requirement ID; each one that was *withdrawn* is in
`REQUIREMENTS.md` §7 with the reason.

- **It never decides to run anything.** Every session traces to a person or to
  a schedule a person created. Autonomous work pickup is deferred by name
  (`REQUIREMENTS.md` §3), and it is the surviving core of the non-goal r9
  reversed.
- **It is not an IDE.** Monaco is a tab type; there are no language servers in
  the client, no extension ecosystem, and no collaborative editing.
- **It has no editor logic, terminal rendering, or language servers
  server-side.** Those are client concerns, and keeping them there is what
  stopped v1's code-server fork happening again.
- **The assistant has no file, network, git or config tools.** Its blast radius
  is: read scrollback, read a curated slice of Vogt, and — after an on-screen
  approval — type into a PTY or make one of four Vogt writes as the approving
  user.
- **The archived GPUI desktop client was not carried across.** It stayed in the
  MyDevEnv2 repository, which is now its archive. A reference to `client/`
  anywhere in this tree names something that is not here.
- **Nothing here is a second backlog.** The engine's outstanding work lives in
  `REQUIREMENTS.md` §7 and `ROADMAP.md`, because an item without a requirement
  is nobody's plan.
