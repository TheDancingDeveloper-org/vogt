# API Contract

This file is the source-of-truth summary for MyDevEnv2's shared wire contract.

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

## Core rules

- All `/api/*` HTTP routes require bearer auth except `/api/config` and
  `/api/push/public-key`.
- `GET /healthz` is public.
- WebSocket attach authenticates with the first text frame:
  `{"type":"auth","token":"..."}`
- WebSocket PTY traffic is binary. Text frames are control messages only.

## Session APIs

- `GET /api/sessions` -> `SessionSummary[]`
- `POST /api/sessions` -> `SessionSummary`
- `GET /api/sessions/:id` -> `SessionDetail`
- `PATCH /api/sessions/:id` -> `OkResponse`
- `POST /api/sessions/:id/kill` -> `OkResponse`
- `POST /api/sessions/:id/input` `{"text": "...", "submit": bool}` -> `OkResponse`
  (writes verbatim to PTY stdin, 64 KiB cap, `submit` appends `\r`; requires
  the `sessions` capability)
- `DELETE /api/sessions/:id` -> `OkResponse`

`SessionSummary` carries an optional `command` field — the explicit command
the session was created with; absent for default-shell sessions.

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

## Attach protocol

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

## Response conventions

Avoid anonymous `serde_json::json!` blobs for stable routes when a named typed
response will do. Current standard small shapes:

- `OkResponse` -> `{"ok": true|false}`
- `WriteFileResponse` -> `{"ok": true, "bytes": <n>}`
