# vogt-session-agent

`vogt-session-agent` is the one-way-connectivity process run inside a Vogt
session pod. It owns the pod's PTY and dials the engine; it never accepts an
inbound connection and never talks to the cluster API.

## Environment

- `VOGT_SESSION_ID`, `VOGT_ENGINE_URL`, `VOGT_AGENT_TOKEN`, and
  `VOGT_SESSION_COMMAND` are required.
- `VOGT_SESSION_CWD`, `VOGT_PTY_COLS`, `VOGT_PTY_ROWS`, and
  `VOGT_AGENT_BUFFER_BYTES` are optional. The replay buffer defaults to 1 MiB.
- For `claude`, `VOGT_CLAUDE_SETTINGS_FILE` (a policy file, `off`, or unset for
  the image's `driven-session-settings.json`) becomes `--settings=<file>`, and
  `VOGT_CLAUDE_MCP_CONFIG` becomes `--strict-mcp-config --mcp-config=<file>`.

The token is sent only as an HTTP Bearer authorization header and is never
logged. The engine URL's `http(s)` scheme is converted to `ws(s)` and the
agent connects to `/api/agent-auth/sessions/{session_id}/agent`.

## Stream protocol

Messages are JSON objects with a `type` discriminator:

- `hello {session_id}` and `resume {cursor}` start a connection.
- `output {offset, data}` carries PTY bytes. `offset` is the absolute byte
  offset of the first byte; offsets are monotonic for the lifetime of a
  session.
- The engine sends `input {data}`, `resize {cols, rows}`, `signal {signal}`
  (`kill` is supported), and `ack {offset}`. Acknowledgements allow the agent
  to discard old replay data.
- `heartbeat {cursor}` keeps idle links alive. The agent sends `exited {status}`
  after the child terminates.

After a disconnect the agent reconnects with exponential backoff, sends its
last cursor, and replays retained bytes. If the requested cursor has fallen
out of the bounded buffer, the engine receives no fabricated data and can
request a fresh snapshot. This makes loss explicit rather than silently
creating a gap.
