# MyDevEnv2

From-scratch redesign of [MyDevEnv](../MyDevEnv). Same goal — a centrally-hosted, Tailscale-accessible dev environment driven from any browser — built cleanly without the accumulated surface area of v1 (code-server fork, multiple half-finished native clients).

- **[INTENT.md](INTENT.md)** — what I'm trying to achieve and why a rewrite
- **[PLAN.md](PLAN.md)** — architecture, components, build order
- **[TOOLING.md](TOOLING.md)** — required tools/toolchains for the dev pod (derived from v1 Dockerfile)

## Status

**Phase 1 (server foundation) — complete.** Single Axum binary at `server/`:

- Bearer-token gated HTTP API for session lifecycle (create / list / get / rename / kill / delete)
- Per-session PTY with a ring-buffer scrollback (default 256 KiB)
- WebSocket attach endpoint with scrollback snapshot replay + live broadcast to multiple clients
- Activity state machine: `idle` / `running` / `waiting-for-input` / `errored`, with regex heuristics on the stripped output tail
- SSE event stream of server-wide session events

Phase 2 (web UI) starts next.

## Running the server

```bash
# 1. Mint a token (≥16 chars)
export MYDEVENV2_TOKEN="$(openssl rand -hex 24)"

# 2. Run
cargo run -p mydevenv2-server -- --bind 127.0.0.1:8910
# or via env:
MYDEVENV2_BIND=127.0.0.1:8910 cargo run -p mydevenv2-server
```

Optional TOML config (`mydevenv2.toml`):

```toml
bind = "0.0.0.0:8910"
token = "..."                  # or use MYDEVENV2_TOKEN env
scrollback_bytes = 262144
default_shell = "/bin/bash"
default_cwd   = "/home/sprooty/Working"
activity_idle_after_ms = 1500
```

Pass with `--config mydevenv2.toml`. CLI flags > env > config file.

## Smoke test with curl + websocat

```bash
TOKEN=$MYDEVENV2_TOKEN
BASE=http://127.0.0.1:8910

# Health
curl -s $BASE/healthz

# Create a session
ID=$(curl -s -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
     -d '{"name":"shell-1"}' $BASE/api/sessions | jq -r .id)

# List
curl -s -H "Authorization: Bearer $TOKEN" $BASE/api/sessions | jq

# Attach over WebSocket (browser-friendly: token via query)
websocat "ws://127.0.0.1:8910/api/sessions/$ID/attach?token=$TOKEN"
# Type, see the shell respond. JSON control frames also work:
#   {"type":"resize","cols":120,"rows":40}

# Stream server events (SSE)
curl -sN -H "Authorization: Bearer $TOKEN" $BASE/api/events

# Kill the child but keep scrollback addressable
curl -s -X POST -H "Authorization: Bearer $TOKEN" $BASE/api/sessions/$ID/kill

# Forget it entirely
curl -s -X DELETE -H "Authorization: Bearer $TOKEN" $BASE/api/sessions/$ID
```

### WebSocket protocol

On attach the server sends:

1. Text frame: `{"type":"snapshot-start","session_id":"...","scrollback_bytes":N,"scrollback_pos":N}`
2. Zero or more binary frames containing scrollback bytes (chunks ≤ 64 KiB)
3. Text frame: `{"type":"snapshot-done"}`
4. Live binary frames from the PTY thereafter

From the client:

- **Binary frames** → written to PTY stdin
- **Text frames** parsed as JSON control:
  - `{"type":"resize","cols":120,"rows":40}` — resize PTY
  - `{"type":"ping"}` — keepalive

If the client falls too far behind the broadcast buffer the server sends `{"type":"lag",...}` and closes the socket; client should reattach (the fresh snapshot will catch them up).

## Tests

```bash
cargo test          # unit tests
cargo test --test integration   # end-to-end (HTTP + WS)
```
