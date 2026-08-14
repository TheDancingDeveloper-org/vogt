# API Contract

This file is the source-of-truth summary for MyDevEnv2's shared wire contract.

`engine/server/src/app.rs` holds the route table and is what actually answers
requests; this file describes it route by route. `tests/test_pwa.py` resolves
every engine path in the shipped PWA against both, so the two cannot drift
apart silently (FR-U8). When they do disagree, this file is the one that is
wrong.

## Contract crate

Rust DTOs live in `engine/contract/` (`mydevenv2-contract`). They were shared
with the legacy native client, which the merge into Vogt left behind in the
MyDevEnv2 repo, so the server is now the only consumer in this tree. Those
types cover:

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

## Core rules

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

## Public routes

- `GET /healthz` -> `OkResponse` — the process is listening. It reads nothing,
  so it stays cheap enough for a container liveness probe.
- `GET /readyz` -> `{"ok": bool, "checks": [{"name","ok","detail","fatal"}]}`
  — five checks: `workspace_root` (readable directory), `state_dir` (writable,
  proved by writing and removing a probe file), `tailscale` (skipped unless
  `TAILSCALE_AUTH_KEY` is set), `gui` (skipped unless `START_SWAY`) and
  `vogt_core`. `200` when every *fatal* check passes, `503` otherwise.
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

## Session APIs

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

### Activity states

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
notification, and re-arms only once the session leaves the state.

`SessionSummary.continuity` is ContextKeeper enrichment and is **absent**, not
null, when there is nothing to say — no sidecar configured, sidecar
unreachable, or no agent session bound to the PTY. It is read from a cache the
ContextKeeper runtime refreshes in the background, so a slow sidecar costs a
stale badge rather than a hung roster. `POST /api/sessions` never enriches: a
brand-new terminal has nothing bound yet, and creating one must not wait on
anything.

## Attach protocol

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

## Events and status

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

## Assistant APIs

All routes 404 unless the server has `MYDEVENV2_ASSISTANT_API_KEY`
provisioned. Mutating routes require the `assistant` token capability. See
`docs/engine/ASSISTANT.md` for the threat model and behavior.

- `POST /api/assistant/message` `{"text": "..."}` ->
  `{"reply": string|null, "pending_action"?: PendingAction, "tool_trace"?: string[]}`
- `POST /api/assistant/actions/:id` `{"approve": bool}` -> same reply shape
- `GET /api/assistant/history` -> `{"transcript": [...], "pending_action"?: ...}`
- `POST /api/assistant/reset` -> `OkResponse`

`PendingAction` is `{"id": uuid, "session_id": uuid, "session_name": string,
"text": string, "submit": bool}` — the exact bytes the assistant wants to type
into a session, awaiting user approval. `GET /api/config` advertises
`assistant_enabled` and `assistant_model` (presence only, never the key).

A transcript entry is `{"role", "text", "tool_trace"?}`. `reply` is null when
the turn paused on a pending action before the model produced any text, which
is the state a client should render as "waiting for you", not as an empty
answer.

## File APIs

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

## Git APIs

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

## GUI APIs

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

## Agent task APIs

Scheduled agent runs. See `docs/engine/AGENT_TASKS.md` for the execution model.
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

`GET /api/status` reports the same artifacts as counts and bytes, so an
operator can see whether a cleanup is worth running before running one.

## Push APIs

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

`PushPreferences` is a per-kind opt-out —
`{waiting_for_input, errored, idle_stall, agent_task_started,
agent_task_notify, quiet_hours}`, each true by default — plus
`quiet_hours: {enabled, start_minute, end_minute, utc_offset_minutes, digest}`.
During quiet hours a notification is counted into a digest instead of sent, and
`queued` in a dispatch count is that outcome rather than a failure.

Notifications the engine sends carry `{kind, session_id, url}` in their data,
where `url` is a PWA route (`/#/t/<session-id>`), so a tap lands on the
terminal that raised it.

## Continuity APIs

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

## Session history APIs

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

## The Vogt front door

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
serves at `/openapi.json`; the repository's `AGENTS.md` and `docs/DESIGN.md`
are the entry points. Only `/api/*` is mapped through the front door, so that
document is reachable at the core and not through this port. Duplicating any of
it here would create a second description to keep in step with a registry that
is already the authority.

## The embedded PWA

`GET /` and `GET /{*path}` serve the Solid bundle compiled into the binary,
with an SPA-style fallback to `index.html` for unknown paths. The catch-all is
merged last so `/healthz`, `/api/*`, `/mcp` and `/ui-legacy` keep priority — a
new route added *after* it would be shadowed by it and answer with the
application shell, which looks like a client-side routing bug rather than a
server one.

## Response conventions

Avoid anonymous `serde_json::json!` blobs for stable routes when a named typed
response will do. Current standard small shapes:

- `OkResponse` -> `{"ok": true|false}`
- `WriteFileResponse` -> `{"ok": true, "bytes": <n>}`

Several routes here still return `serde_json::Value` — the push routes, the
continuity proxy, `DELETE /api/history/:id`, `DELETE /api/agent-tasks/:id`.
The continuity ones are that way on purpose, because the shapes are
ContextKeeper's. The rest are the standard above not yet applied.

## Not covered here

- **Vogt's operations.** See the front door section: the registry and its
  OpenAPI document are the authority for everything under `/api/vogt/`.
- **Field-by-field type definitions.** The Rust types in
  `engine/contract/src/lib.rs` and the handler modules are the exact shapes;
  this file names them and describes the behaviour a client cannot infer from
  a struct.
- **Configuration.** Which tokens exist, which capabilities they hold, and
  every value named above as a default: `engine/README.md` and
  `engine/server/src/config.rs`. `docs/CONFIG.md` is the *core's* configuration
  and does not describe this process.
- **The assistant's tool loop and threat model** — `docs/engine/ASSISTANT.md`.
- **Agent task scheduling and execution** — `docs/engine/AGENT_TASKS.md`.
- **ContextKeeper's own API.** Only the allow-listed subset above is proxied;
  the sidecar's contract is the sidecar's.
