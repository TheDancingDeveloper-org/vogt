# MyDevEnv2

From-scratch redesign of [MyDevEnv](../MyDevEnv). Same goal — a centrally-hosted, Tailscale-accessible dev environment driven from any browser — built cleanly without the accumulated surface area of v1 (code-server fork, multiple half-finished native clients).

- **[INTENT.md](INTENT.md)** — what I'm trying to achieve and why a rewrite
- **[PLAN.md](PLAN.md)** — architecture, components, build order
- **[TOOLING.md](TOOLING.md)** — required tools/toolchains for the dev pod (derived from v1 Dockerfile)
- **[client/README.md](client/README.md)** — archived notes for the deprecated native desktop client
- **[deploy/KOMODO.md](deploy/KOMODO.md)** — production stack and deploy notes

## Current status

MyDevEnv2 is live as the Komodo stack `prod-mydevenv2`, with desired state in
the `indexarr/ops` repo at `personal/mydevenv2/`. The production image is
`repo.indexarr.net/indexarr/mydevenv2`, served on port `8910`, with the PWA and
API at `https://mydevenv2.sprooty.com` through Caddy. The direct Node B health
endpoint currently returns `{"ok":true}`; the public URL may be Caddy
basic-auth gated before requests reach the app.

The repository now has four core repo components and three supported product
surfaces:

- `contract/` — shared Rust wire DTOs used by the server and retained legacy
  native client code.
- `server/` — Rust/Axum server plus embedded Solid PWA.
- `web/` — Solid/Vite PWA served by the Rust binary.
- `mobile/` — Capacitor 8 Android shell that loads the deployed PWA.
- `client/` — deprecated legacy GPUI desktop client source, retained only as
  reference while the supported product remains the PWA and Android shell.

CI is split across `.woodpecker/`:

- `.woodpecker/server.yml` runs server fmt/clippy/test, web typecheck, debug
  APK build, Docker buildx, and Komodo deploy for non-`client/**` pushes to
  `main`.

As of July 7, 2026, the native desktop client is deprecated. No client CI or
release workflow remains active; `client-v0.1.4` is the last verified native
client release kept in Forgejo for historical reference.

## Server + PWA phases

**Phase 1 (server foundation) — complete.** Single Axum binary at `server/`:

- Bearer-token gated HTTP API for session lifecycle (create / list / get / rename / kill / delete)
- Per-session PTY with a ring-buffer scrollback (default 4 MiB)
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

**Phase 5 (GUI tab + dev-pod packaging) — code-complete and deployed; GUI stream disabled by default.**

- Server: `POST /api/gui/launch`, `GET /api/gui/processes`, `POST /api/gui/kill?pid=`. Optional `via_sway` prefixes with `swaymsg exec --`. `GET /api/config` (public) returns `gui_stream_url` and build feature flags for the web UI.
- Client: `gui` tab kind iframing the configured stream URL; toolbar to launch arbitrary GUI commands; running-processes list with kill buttons. Deep-link `/#/gui`.
- Packaging: `Dockerfile` (multi-stage: web bundle → Rust release → Ubuntu 26.04 runtime with `TOOLING.md`, Sway, Selkies-GStreamer, Tailscale userspace, Docker CLI, Infisical, GitHub CLI, and the embedded PWA). `deploy/entrypoint.sh` orchestrates Tailscale → optional Sway → auth validation → server.
- Production: the Komodo stack exists and is deployed. `START_SWAY=0` and `GUI_STREAM_URL=""` keep the GUI stream off until Selkies is wired and verified inside the pod.

**Phase 6 (push + Android Capacitor APK) — code-complete; runtime push delivery pending real-device verification.**

- Server: VAPID web-push (any modern browser PushManager subscription, including installed-PWA iOS Safari 16.4+) + FCM HTTP v1 (native Capacitor tokens). Service-account JWT → OAuth2 with token caching. Subscriptions persist as JSON under `state_dir`; auto-prune on 404/410.
- Server routes: `POST /api/push/subscribe`, `POST /api/push/unsubscribe`, `GET /api/push/list`, `POST /api/push/test`, `GET /api/push/public-key` (public — no token needed).
- Activity watcher: fires push to all subscriptions when any session enters `waiting-for-input`.
- Web: `/sw.js` + `/manifest.webmanifest` for PWA install + push event handling. Installed PWAs show an explicit offline fallback page instead of pretending to support disconnected use. Settings modal gains "Enable push" / "Send test" with current-permission visibility.
- Mobile: `mobile/` Capacitor 8 Android wrap (`com.sprooty.mydevenv2`). WebView loads `https://mydevenv2.sprooty.com` directly so UI updates ship without rebuilding the APK. `@capacitor/push-notifications` registers a native FCM token at first launch; the same `/api/push/subscribe` endpoint accepts both transports.
- CI: `mobile-apk` builds the debug APK on pushes handled by `.woodpecker/server.yml` and uploads it to the Forgejo release tag `apk-latest` as `mydevenv2-debug.apk`.

Phase 7 (Android emulator KVM VM) remains.

**June 2026 UX uplift — code-complete.**

- Command palette (`Ctrl/Cmd+K`) opens sessions, files, recent files, settings, GUI, git, shortcuts, and history search. Prefix a query with `>` to search archived session output.
- Session history archives exited PTY sessions into SQLite under `state_dir`, writes raw logs under `state_dir/session-logs`, indexes ANSI-stripped output with FTS5, and exposes list/search/get/delete routes under `/api/history/*`.
- IDE layout mode now embeds the real workspace file tree and keeps non-editor tabs usable. When an editor tab is active, the editor workspace can split files side-by-side or stacked, resize panes by dragging, and keeps dirty-state tracking tied to the real editor tabs.
- Editor quality-of-life features include breadcrumbs, file icons, file-tree filtering, recent files, minimap toggle, and persisted layout/editor preferences.
- Session templates, custom template editing, terminal themes, bookmarks, and keyboard shortcut reference are available from the PWA settings/commands.

**July 2026 workflow uplift — code-complete.**

- Weather and daily briefing were removed; recurring work now lives in a first-class Tasks tab backed by `/api/agent-tasks`, with schedule management, run history, and session-open actions.
- Session templates grew into richer workspace presets with tags, repo/path matching, placeholder expansion, and direct launch from the command palette or file tree.
- File and git surfaces now cover common mutating workflows: move/delete/mkdir/duplicate in the workspace tree, plus stage/unstage/discard/commit/checkout in the git tab.
- Saved workspace layouts capture browser-local tab sets and layout mode, and grouped terminal workspaces now support multiple panes plus broadcast input.
- History gained filtering, archived-output search, pinning, replay-tail previews, and raw-log export for post-run inspection.

---

## Native desktop client

The GPUI desktop client under `client/` is deprecated as of July 7, 2026.
MyDevEnv2 now treats the browser/PWA as the primary desktop experience, with
the Android app remaining a thin native shell over the same web surface.

The `client/` tree is kept only as legacy reference while a future thin Windows
wrapper decision remains open. It is no longer released, no longer covered by
active Woodpecker workflows, and should not be treated as a supported product
surface.

See [client/README.md](client/README.md) for the archived native-client notes.

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
scrollback_bytes = 4194304
default_shell = "/bin/bash"
default_cwd   = "/home/sprooty/Working"
workspace_root = "/home/sprooty/Working"
activity_idle_after_ms = 1500
state_dir = "/home/sprooty/.local/share/mydevenv2"
vapid_subject = "mailto:admin@example.invalid"
allowed_origins = [
  "https://mydevenv2.sprooty.com",
  "http://localhost:5173",
  "http://127.0.0.1:5173",
]
auto_agent_auth = false
agent_auth_helper = "/usr/local/bin/mydevenv2-agent-auth"
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

# Attach over WebSocket. The first frame must authenticate:
websocat "ws://127.0.0.1:8910/api/sessions/$ID/attach"
# First paste:
#   {"type":"auth","token":"'"$TOKEN"'"}
# Then type and see the shell respond. JSON control frames also work:
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

- First text frame must be `{"type":"auth","token":"..."}`. Legacy
  `?token=...` WebSocket auth still exists only for older clients and should
  not be used for new code because URLs land in proxy/access logs.
- **Binary frames** → written to PTY stdin
- **Text frames** parsed as JSON control:
  - `{"type":"resize","cols":120,"rows":40}` — resize PTY
  - `{"type":"ping"}` — keepalive

If the client falls too far behind the broadcast buffer the server sends `{"type":"lag",...}` and closes the socket; client should reattach (the fresh snapshot will catch them up).

## Tests

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test                       # server unit + integration tests
cargo test -p mydevenv2-contract # shared wire-contract tests
cd web && pnpm typecheck         # PWA TypeScript check

# Legacy native client checks (deprecated surface; no active CI):
cd client
cargo fmt --check
cargo clippy --no-default-features --all-targets -- -D warnings
cargo test --no-default-features
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
