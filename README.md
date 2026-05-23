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

**Phase 2 (web UI MVP) — complete.** Solid + Vite + TS PWA at `web/`, embedded into the server binary via `rust-embed`:

- Responsive shell — tab strip + main pane + drawer; three breakpoints, one component tree
- xterm.js terminal tab attached over WebSocket with snapshot replay
- Mobile modifier-key row (Esc / Tab / Ctrl (sticky) / arrows / `/` / `|` / `~` / Enter)
- Per-tab activity badges driven by SSE; pulse animation for `running` and `waiting-for-input`
- Deep-link URLs via HashRouter (`/#/t/<session-id>`)
- Settings modal stores bearer token + (optional) backend base URL in localStorage

**Phase 3 (files + editor) — complete.**

- Server: `workspace_root`-scoped file API — `GET /api/dir`, `GET /api/tree`, `GET/PUT /api/files`, `GET /api/search` (via ripgrep). Path-traversal guarded with strict component checks; binary detection; 5 MiB read cap.
- Client: file tree in the drawer with lazy expand; Monaco editor as a new tab kind, lazily imported so the first paint stays at ~93 KB gz; Ctrl/Cmd+S saves; per-tab dirty indicator; tab state persisted to localStorage.
- Deep-link route `/#/e/<path>` opens an editor tab on that file.

**Phase 4 (git tab) — complete, read-only.**

- Server: `GET /api/git/{status,diff,log,branch}` — auto-detects repo root by walking up from the supplied `?repo=` relative path. Shells out to `git`.
- Client: new `git` tab kind. Status pane groups entries by kind (conflicted / staged / modified / renamed / deleted / untracked); click a path to load a Monaco diff editor (HEAD vs working tree). Recent commits below; branch + ahead/behind chip up top.
- Deep-link route `/#/g/<repo>` opens the git tab for that repo.

**Phase 5 (GUI tab + dev-pod packaging) — code-complete; pending real-pod verification.**

- Server: `POST /api/gui/launch`, `GET /api/gui/processes`, `POST /api/gui/kill?pid=`. Optional `via_sway` prefixes with `swaymsg exec --`. `GET /api/config` (public) returns `gui_stream_url` for the web UI to iframe.
- Client: new `gui` tab kind iframing the configured stream URL; toolbar to launch arbitrary GUI commands; running-processes list with kill buttons. Deep-link `/#/gui`.
- Packaging: `Dockerfile` (multi-stage: web bundle → rust release → Ubuntu 26.04 runtime with all of TOOLING.md + Sway + Selkies-GStreamer + Tailscale userspace). `deploy/docker-compose.yml` ready for the ops repo, `deploy/KOMODO.md` with one-time stack-creation steps, `deploy/entrypoint.sh` orchestrates Tailscale + optional Sway + server.
- CI: `.woodpecker.yml` runs fmt/clippy/test/web-typecheck, builds the image with `:latest` + `:${CI_COMMIT_SHA}`, then triggers `komodo-deploy` against `prod-mydevenv2` in `ops/personal/mydevenv2/`.

What's still on the user's plate before Phase 5 is fully verifiable:
1. Create the Forgejo container-registry credentials (or confirm `git_auth_token` works) for the first `build-and-push`.
2. Add `personal/mydevenv2/docker-compose.yml` to the ops repo and create the Komodo stack (see `deploy/KOMODO.md`).
3. Mint `MYDEVENV2_TOKEN` + `HOMELAB_MYDEVENV2_TAILSCALE_AUTH_KEY` in Infisical and add to the Komodo stack `environment`.
4. Decide the workspace bind-mount strategy on Node B (NFS / Syncthing / direct).
5. To actually use the GUI tab, set `START_SWAY=1` and `GUI_STREAM_URL=…` once Selkies is reachable inside the pod.

Phase 6 (web push + Android Capacitor wrap, sideloaded APK — iOS deferred) and Phase 7 (Android emulator KVM VM) remain.

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
cargo test                       # unit tests
cargo test --test integration    # end-to-end (HTTP + WS)
cd web && pnpm typecheck         # PWA TypeScript check
```

## Building the embedded PWA

The Rust server `cargo build` embeds whatever is in `web/dist/` at compile
time. To refresh:

```bash
cd web && pnpm install && pnpm build
cd .. && cargo build --release
```

For UI development, run the server on its native port and Vite dev server
in parallel — Vite proxies `/api` and the WS endpoint to the backend:

```bash
# terminal 1
MYDEVENV2_TOKEN=$(openssl rand -hex 24) cargo run -p mydevenv2-server -- --bind 127.0.0.1:8910

# terminal 2
cd web && pnpm dev
# → http://127.0.0.1:5173, paste the token into Settings (⚙)
```
