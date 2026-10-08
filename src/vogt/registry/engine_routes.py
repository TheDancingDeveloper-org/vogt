"""The session engine's HTTP routes, each answered for on MCP.

The core's own operations reach MCP by construction: one registry generates
REST, MCP and the CLI. The engine's routes do not — they are Rust handlers
the PWA calls directly — so an engine route the GUI gained could leave agents
with no way to do what a person can (renaming a session was exactly that).
This module is the parity rule for that half:

- `ENGINE_COUNTERPARTS` names, for each engine route an agent should be able
  to reach, the registry operation that is its MCP counterpart — the core
  operation that proxies it, or one that serves the same need.
- `ENGINE_ONLY` names every other engine route with the reason it has no
  counterpart.

`tests/test_engine_parity.py` reads the engine's router and fails when a route
is in neither table, when an entry names a route that no longer exists, or
when a counterpart is not a registered operation on MCP. Keys are
`"METHOD /path"` exactly as `engine/server/src/app.rs` writes the path.
"""

from __future__ import annotations

from collections.abc import Mapping

#: Engine route -> the registry operation that is its MCP counterpart.
ENGINE_COUNTERPARTS: Mapping[str, str] = {
    "GET /api/sessions": "session.list",
    "POST /api/sessions": "session.start",
    # The raw scrollback the GUI paints a terminal from; an agent reads the
    # rendered screen, or the log for more than one screen.
    "GET /api/sessions/{id}": "session.screen",
    "PATCH /api/sessions/{id}": "session.rename",
    "DELETE /api/sessions/{id}": "session.remove",
    "GET /api/sessions/sweep": "session.sweep",
    "GET /api/sessions/{id}/screen": "session.screen",
    "GET /api/sessions/{id}/replies": "session.last_reply",
    "POST /api/sessions/{id}/kill": "session.stop",
    "POST /api/sessions/{id}/input": "session.input",
    "GET /api/sessions/{id}/wait": "session.wait",
    "POST /api/sessions/{id}/blocked": "session.report_blocked",
    "POST /api/sessions/{id}/answer": "session.answer",
    "POST /api/sessions/{id}/hibernate": "session.hibernate",
    "POST /api/sessions/{id}/wake": "session.wake",
    "POST /api/sessions/{id}/keep-awake": "session.keep_awake",
    "POST /api/sessions/{id}/role": "session.set_role",
    "POST /api/sessions/{id}/work-item": "session.bind_work",
    "GET /api/sessions/{id}/grants": "session.grant_list",
    # A grant reaches the engine only once a person approves it.
    "POST /api/sessions/{id}/grants": "session.grant_decide",
    "DELETE /api/sessions/{id}/grants/{grant_id}": "session.grant_revoke",
    "GET /api/status": "engine.status",
    "GET /api/auth/check": "auth.whoami",
    "GET /api/agent-clis": "agent_cli.list",
    "POST /api/agent-clis/{tool}": "agent_cli.update",
    "GET /api/history/sessions": "session.history_list",
    "GET /api/history/search": "session.search_output",
    "GET /api/history/{id}": "session.history_list",
    "GET /api/history/{id}/log": "session.log_tail",
    "GET /api/history/{id}/download": "session.log_tail",
    # Quick chats (WI-1097).
    "GET /api/chats": "chat.list",
    "POST /api/chats": "chat.create",
    "GET /api/chats/{id}": "chat.get",
    "POST /api/chats/{id}/messages": "chat.send",
    # Only a person answers; an agent's chat.decide is refused, as its
    # session.answer to a permission prompt is.
    "POST /api/chats/{id}/approvals/{approval_id}": "chat.decide",
    "POST /api/chats/{id}/model": "chat.set_model",
    "POST /api/chats/{id}/interrupt": "chat.interrupt",
    "POST /api/chats/{id}/archive": "chat.archive",
    "POST /api/chats/{id}/promote": "chat.promote",
    # The live event stream; chat.send waits for the reply and chat.get
    # reads the same record, which is MCP's request/response shape for it.
    "GET /api/chats/{id}/events": "chat.get",
}

_PROBE = (
    "A liveness/readiness probe or a path the front door forwards to the "
    "core's own health routes; a supervisor's concern, and `status` / "
    "`instance.diagnostics` are the agent's view of the same thing."
)
_BOOTSTRAP = (
    "Browser bootstrap, answered before the caller holds any credential "
    "(public config, first-run install, password login). An MCP caller "
    "already holds a token; `auth.whoami` and `connect` are its equivalents."
)
_FRONT_DOOR = (
    "The front door to the core's own REST or MCP surface. Every operation "
    "behind it is a registry operation, on MCP by construction."
)
_FALLBACK = (
    "Routing fallbacks: the `/api` namespace's 404 and the embedded PWA's "
    "assets. Not operations."
)
_PUSH = (
    "Device notification plumbing: a subscription is a browser's or phone's "
    "push endpoint, which an agent does not have, and test/flush operate that "
    "device fan-out. Agents read the same signal through `notifications` and "
    "`inbox.list`."
)
_ASSISTANT = (
    "The in-app assistant is itself an agent loop over these MCP tools whose "
    "every effector waits for a person's on-screen approval. Letting an MCP "
    "agent message it, resolve its actions or reset it would let one agent "
    "approve another's actions — the person-only gate this surface exists "
    "for. Speech (stt/tts, the live call) is audio for the person's device."
)
_PWA_ONLY = (
    "The PWA's own transport: its diagnostics log, the server-sent event "
    "stream and the terminal WebSocket (which also carries resize). MCP is "
    "request/response; `events.list`, `session.wait`, `session.screen` and "
    "`session.input` are its counterparts for the same information."
)
_WORKSPACE = (
    "The workspace file and git browser. An agent runs in a session inside "
    "the workspace and has the files and git directly. Over MCP these would "
    "give any remote token holder a tree read/write channel that the engine "
    "gates on `sessions`, `filesystem-write` and `git-write` (WI-1020) and "
    "that the core's own engine credential deliberately does not hold."
)
_GUI = (
    "`gui-control` is arbitrary code execution as the pod user (ENGINE.md "
    "§auth). The core's engine credential does not hold it, and an agent "
    "that needs a process starts a session."
)
_AGENT_TASKS = (
    "Scheduled agent tasks. Writes need `agent-tasks-write`, which is "
    "arbitrary code execution and is deliberately not held by the core's "
    "engine credential, and answering a task's gate is the only path that "
    "approves it. Putting them on MCP is an operator decision about that "
    "credential, tracked as WI-1095; until then agents start sessions."
)
_HISTORY_WRITE = (
    "Deleting archived session history. The archive is the record of what "
    "sessions — agents included — did, so erasing it is a person's call, and "
    "`history-write` is deliberately not held by the core's engine "
    "credential."
)
_CHAT_GATE = (
    "A quick chat's approval gate: the chat's own agent's PreToolUse hook "
    "asks it, with the per-process token the engine gave that agent, whether "
    "a tool call may run. Not a person's or another agent's call; the "
    "decision itself is `chat.decide`, which only a person may make."
)
_SESSION_SELF = (
    "A session's own calls with its per-session broker token (secret "
    "fetch/store, launch report, its conversation, its grants), or the "
    "engine-side record of the conversation a session runs. Made by the "
    "session's launcher and hooks, not by a person or another agent."
)

#: Engine route -> why it has no MCP counterpart.
ENGINE_ONLY: Mapping[str, str] = {
    "GET /healthz": _PROBE,
    "GET /readyz": _PROBE,
    "GET /version": _PROBE,
    "GET /connection-info": _PROBE,
    "GET /health/ready": _PROBE,
    "GET /health/live": _PROBE,
    "GET /api/config": _BOOTSTRAP,
    "GET /api/install/status": _BOOTSTRAP,
    "POST /api/install/bootstrap": _BOOTSTRAP,
    "POST /api/auth/login": _BOOTSTRAP,
    "ANY /api/vogt": _FRONT_DOOR,
    "ANY /api/vogt/": _FRONT_DOOR,
    "ANY /api/vogt/{*path}": _FRONT_DOOR,
    "ANY /mcp": _FRONT_DOOR,
    "ANY /mcp/": _FRONT_DOOR,
    "ANY /mcp/{*path}": _FRONT_DOOR,
    "ANY /api": _FALLBACK,
    "ANY /api/": _FALLBACK,
    "ANY /api/{*path}": _FALLBACK,
    "GET /": _FALLBACK,
    "GET /{*path}": _FALLBACK,
    "GET /api/push/public-key": _PUSH,
    "POST /api/push/subscribe": _PUSH,
    "POST /api/push/update": _PUSH,
    "POST /api/push/unsubscribe": _PUSH,
    "GET /api/push/list": _PUSH,
    "POST /api/push/test": _PUSH,
    "POST /api/push/flush-digests": _PUSH,
    "POST /api/assistant/message": _ASSISTANT,
    "POST /api/assistant/actions/{id}": _ASSISTANT,
    "PATCH /api/assistant/actions/{id}": _ASSISTANT,
    "GET /api/assistant/history": _ASSISTANT,
    "GET /api/assistant/log": _ASSISTANT,
    "POST /api/assistant/reset": _ASSISTANT,
    "POST /api/assistant/stt": _ASSISTANT,
    "POST /api/assistant/tts": _ASSISTANT,
    "GET /api/assistant/call": _ASSISTANT,
    "POST /api/client-log": _PWA_ONLY,
    "GET /api/events": _PWA_ONLY,
    "GET /api/sessions/{id}/attach": _PWA_ONLY,
    "GET /api/files": _WORKSPACE,
    "PUT /api/files": _WORKSPACE,
    "PUT /api/files/upload": _WORKSPACE,
    "POST /api/files/op": _WORKSPACE,
    "GET /api/files/download": _WORKSPACE,
    "GET /api/dir": _WORKSPACE,
    "GET /api/tree": _WORKSPACE,
    "GET /api/search": _WORKSPACE,
    "GET /api/search/files": _WORKSPACE,
    "GET /api/git/status": _WORKSPACE,
    "GET /api/git/diff": _WORKSPACE,
    "GET /api/git/log": _WORKSPACE,
    "GET /api/git/branch": _WORKSPACE,
    "POST /api/git/op": _WORKSPACE,
    "POST /api/gui/launch": _GUI,
    "GET /api/gui/processes": _GUI,
    "POST /api/gui/kill": _GUI,
    "GET /api/agent-tasks": _AGENT_TASKS,
    "POST /api/agent-tasks": _AGENT_TASKS,
    "GET /api/agent-tasks/{id}": _AGENT_TASKS,
    "PATCH /api/agent-tasks/{id}": _AGENT_TASKS,
    "DELETE /api/agent-tasks/{id}": _AGENT_TASKS,
    "POST /api/agent-tasks/{id}/pause": _AGENT_TASKS,
    "POST /api/agent-tasks/{id}/resume": _AGENT_TASKS,
    "POST /api/agent-tasks/{id}/run": _AGENT_TASKS,
    "POST /api/agent-tasks/{id}/steer": _AGENT_TASKS,
    "POST /api/agent-tasks/{id}/gates/{gate_id}/answer": _AGENT_TASKS,
    "POST /api/agent-tasks/artifacts/cleanup": _AGENT_TASKS,
    "POST /api/history/cleanup": _HISTORY_WRITE,
    "DELETE /api/history/{id}": _HISTORY_WRITE,
    "POST /api/sessions/{id}/conversation": _SESSION_SELF,
    "POST /api/agent-auth/fetch/{var}": _SESSION_SELF,
    "POST /api/agent-auth/store/{var}": _SESSION_SELF,
    "POST /api/agent-auth/launch-report": _SESSION_SELF,
    "POST /api/agent-auth/conversation": _SESSION_SELF,
    "GET /api/agent-auth/grants": _SESSION_SELF,
    "POST /api/chats/{id}/gate": _CHAT_GATE,
}
