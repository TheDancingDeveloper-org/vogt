# MyDevEnv2 Agent Guide

This file is the canonical project guidance for AI agents working in MyDevEnv2.
The workspace root guidance is [`/home/sprooty/Working/AGENTS.md`](/home/sprooty/Working/AGENTS.md).
`CLAUDE.md` is kept only as a compatibility pointer back to this file.

Before changing files here:

- Read `/home/sprooty/Working/AGENTS.md` for workspace-wide infrastructure, CI/CD, secrets, commit rules, and service access paths.
- Follow this file for MyDevEnv2-specific build, deploy, and runtime guidance.
- Do not treat `CLAUDE.md` as source of truth; it should only point agents here.

## 1. Quick reference

| | |
|---|---|
| Type | Rust (Axum) + Solid/Vite PWA (embedded via `rust-embed`) + Capacitor Android wrap |
| Repo | `repo.indexarr.net/indexarr/MyDevEnv2` |
| CI pipeline | `.woodpecker.yml` — fmt/clippy/test -> web-typecheck -> mobile-apk -> buildx -> komodo-deploy |
| Deploys to | Komodo stack `prod-mydevenv2` (ops repo path `personal/mydevenv2/`) — target periphery is the one running mydevenv2.sprooty.com (see ops repo) |
| Image | `repo.indexarr.net/indexarr/mydevenv2` (`:latest` + `:<sha>`) |
| Runtime port(s) | `8910/tcp` (HTTP API + WebSocket attach + SSE; PWA served from same port) |
| Public URL | `https://mydevenv2.sprooty.com` |
| DB / state | None (Postgres-less by design). Sessions in-memory; push subscriptions persisted as JSON under `state_dir`. |
| Secrets used at runtime | `MYDEVENV2_TOKEN` (API bearer), `HOMELAB_MYDEVENV2_TAILSCALE_AUTH_KEY` (Tailscale userspace), `HOMELAB_MYDEVENV2_INFISICAL_CLIENT_ID` / `HOMELAB_MYDEVENV2_INFISICAL_CLIENT_SECRET` (required production agent service auth), `HOMELAB_MYDEVENV2_VAPID_*` + FCM service account JSON for push — all in Infisical `apps` and pasted into the Komodo stack `environment` |

From-scratch rewrite of `../MyDevEnv`: same centrally-hosted, Tailscale-accessible dev environment goal, built without the v1 surface area of a code-server fork and multiple half-finished native clients. Active status: Phase 1-6 code-complete; Phase 7 (KVM-backed Android emulator VM) pending.

## 2. Architecture

Single binary, multi-tab PWA. `rust-embed` bakes the Solid PWA bundle into the Rust release at compile time.

```text
Browser / iOS PWA / Android Capacitor APK
   |   HTTPS + WSS via Caddy at mydevenv2.sprooty.com
   v
Axum server (mydevenv2-server) — bind :8910
   |-- /api/sessions/*             PTY lifecycle (bearer-token)
   |-- /api/sessions/:id/attach    WebSocket (snapshot replay -> live binary frames)
   |-- /api/events                 SSE — server-wide session state changes
   |-- /api/dir, /api/tree, /api/files, /api/search   workspace_root-scoped, ripgrep
   |-- /api/git/{status,diff,log,branch}              shells out to git
   |-- /api/push/*                 VAPID web-push + FCM HTTP v1
   |-- /api/gui/*                  swaymsg-driven GUI launcher (Phase 5)
   `-- /                            embedded PWA (rust-embed)
```

See `PLAN.md` and `INTENT.md` for design decisions and rationale.

## 3. Build / test / run

```bash
# Run server (mint a token first, >=16 chars)
export MYDEVENV2_TOKEN="$(openssl rand -hex 24)"
cargo run -p mydevenv2-server -- --bind 127.0.0.1:8910

# Tests
cargo test --all                  # unit + integration (HTTP + WS)
cd web && pnpm typecheck          # PWA TS check

# Refresh embedded PWA bundle (cargo bakes web/dist/ at build time)
cd web && pnpm install && pnpm build
cd .. && cargo build --release

# UI development (Vite proxies /api + WS to the backend):
# terminal 1: cargo run -p mydevenv2-server -- --bind 127.0.0.1:8910
# terminal 2: cd web && pnpm dev   -> http://127.0.0.1:5173
```

Optional TOML config at `mydevenv2.toml` (CLI > env > config); see README for keys.

## 4. Deployment

Pipeline non-standard bits:

- Custom `clone:` block uses `git_auth_token` because the default OAuth clone started 403'ing at pipeline #17. Reference pattern in root Woodpecker pitfalls #2 / #6.
- `sccache` uses Redis on Node B (`100.92.54.45:6380`) for `fmt` / `clippy` / `test` steps. Do not `apt install sccache`; Debian's package lacks Redis support. The pipeline fetches the GitHub release binary v0.10.0.
- `mkdir -p web/dist && touch web/dist/.placeholder` is used in `clippy` and `test` steps so `rust-embed` compiles before `build-and-push` produces the real bundle.
- `mobile-apk` builds the Capacitor debug APK and uploads it to Forgejo releases API tag `apk-latest`, not the generic-package registry. Idempotence is delete-then-create using `scripts/forgejo-api.sh` from `indexarr/ops`.
- `cimg/android` runs as `circleci` (UID 3434) but the workspace was cloned by `alpine/git` as root. The pipeline does `sudo chown -R circleci:circleci .` before pnpm.
- Komodo deploy pins the SHA in `ops/personal/mydevenv2/docker-compose.yml` through standard `scripts/komodo-deploy.sh` (`STACK_NAME=prod-mydevenv2 STACK_DIR=personal/mydevenv2`).

Runtime container: multi-stage `Dockerfile` produces an Ubuntu 26.04 runtime carrying the full `TOOLING.md` toolchain set, Sway, Selkies-GStreamer, Tailscale userspace, neutral service CLIs (`infisical`, `gh`, `git`, `curl`), and the embedded PWA. `deploy/entrypoint.sh` orchestrates Tailscale -> optional Sway -> server.

Codex and Claude are deliberately not installed by container bootstrap. Optional AI clients are user-managed. Production default-shell sessions are authenticated through `mydevenv2-agent-auth`; explicit-command sessions can invoke the helper directly.

Outstanding before Phase 5 GUI tab is fully verifiable: items 1-6 in `README.md` under "What's still on the user's plate".

## 5. Agent service authentication

MyDevEnv2 provides a neutral credential bridge for agents:

```bash
mydevenv2-agent-auth check
mydevenv2-agent-auth run -- <command>
mydevenv2-agent-auth shell
```

With `MYDEVENV2_AUTO_AGENT_AUTH=1`, sessions created without an explicit
`command` automatically use `mydevenv2-agent-auth shell`. Explicit-command
sessions are not wrapped.

These commands require the read-only Infisical Universal Auth credentials `INFISICAL_CLIENT_ID` and `INFISICAL_CLIENT_SECRET`, injected by the Komodo stack from:

- `HOMELAB_MYDEVENV2_INFISICAL_CLIENT_ID`
- `HOMELAB_MYDEVENV2_INFISICAL_CLIENT_SECRET`

The helper fetches Forgejo, Woodpecker, GitHub, and Komodo credentials into the child-process environment only. Do not persist fetched tokens in the workspace, shell profiles, local Git config, or logs.

Use `mydevenv2-agent-auth check` to prove authenticated access to:

- Forgejo API: `https://repo.indexarr.net/api/v1/user`
- Forgejo git: `https://repo.indexarr.net/indexarr/ops.git`
- Woodpecker API: `https://ci.indexarr.net/api/user`
- GitHub org API: `gh api orgs/AusAgentSmith-org`
- Komodo API: `http://100.92.54.45:3011/read`

The production compose requires both identity values and validates the broker
at startup. If either is missing or invalid, deployment must fail rather than
starting a healthy-looking server without service access.

### June 2026 auth recovery note

Root cause: the running MyDevEnv2 container was created on June 12, 2026 at
03:05 UTC with empty `INFISICAL_CLIENT_ID` / `INFISICAL_CLIENT_SECRET` values,
even though Komodo later held valid identity credentials. Existing sessions
inherited PID 1's stale environment. Default sessions also launched plain Bash,
so only explicit `mydevenv2-agent-auth shell` sessions loaded Forgejo,
Woodpecker, GitHub, Komodo, and Docker access. SSH was intentionally not
configured, and the container user lacked the host Docker socket group
(`984`).

Fixes applied here:

- Default interactive sessions can auto-wrap through the auth broker
  (`MYDEVENV2_AUTO_AGENT_AUTH=1`).
- Production compose requires the Infisical identity credentials, adds Docker
  socket GID `984`, and sets authenticated shells as the default.
- Startup can require and validate agent authentication before serving traffic.

Validation for the recovery covered Infisical, Forgejo, Woodpecker, GitHub,
Komodo, Docker access, Clippy, and the full Rust test suite.

## 6. Workspace synchronization

MyDevEnv2 does not expose an SSH server. Its workspace is a Node B host bind mount:

- Node B host path: `/mnt/2tnvme/docker/volumes/mydevenv2/workspace`
- Container path: `/home/sprooty/Working`
- Local workspace path: `/home/sprooty/Working`

The workspace-root `mutagen.yml` must therefore target:

```yaml
beta: "sprooty@100.92.54.45:/mnt/2tnvme/docker/volumes/mydevenv2/workspace"
```

To recover sync on a fresh client:

1. Confirm certificate-based Node B access with `ssh -o BatchMode=yes sprooty@100.92.54.45 hostname`.
2. Confirm the live mount with `docker inspect mydevenv2` on Node B; do not target the old MyDevEnv v1 endpoint `root@dev.sprooty.com:/config/workspace`.
3. From `/home/sprooty/Working`, run `mutagen project terminate` and `mutagen project start`.
4. Verify both endpoints are connected with `mutagen sync list --long`. The initial full-workspace scan can take several minutes.

The sync mode is `two-way-safe`. VCS metadata, machine-local Claude settings, generated Claude worktrees, virtualenvs, and dependency/build directories are intentionally ignored by `mutagen.yml`. Real source conflicts must not be resolved by resetting the session or selecting one side without first preserving and reviewing both versions.

## 7. Rules for AI agents

- Bearer token gates everything except `/healthz`, `/api/push/public-key`, and `/api/config`. Do not add new public routes without thinking about CSRF; the PWA stores the token in `localStorage` and sends it via `Authorization:` header. WebSocket currently falls back to a `?token=` query param.
- WebSocket attach protocol is ordered: `snapshot-start` text frame -> <=64 KiB binary scrollback chunks -> `snapshot-done` text -> live binary. Client text frames must be JSON (`{"type":"resize"|"ping"|...}`); binary frames are written verbatim to PTY stdin. Lag causes the server to send `{"type":"lag",...}` and close; the client should reattach.
- `workspace_root` is the boundary for all file APIs. Path-traversal is strict-component-checked, binary detection runs, and reads cap at 5 MiB. Do not introduce a file endpoint that bypasses these checks.
- Activity state machine drives push delivery: `idle` / `running` / `waiting-for-input` / `errored`. Push fires on entry to `waiting-for-input`. The heuristic is regex on stripped output tail; adjust with care because false positives become phone notifications.
- PWA bundle is baked at compile time. A `cargo build` without a fresh `pnpm build` ships stale frontend. The `web/dist/.placeholder` trick is for CI lint stages only; production builds must produce a real bundle.
- Push subscriptions auto-prune on 404/410 from the push endpoint. Do not add manual retry/cleanup on top; the prune is the cleanup.
- Mobile WebView loads `https://mydevenv2.sprooty.com` directly. UI changes ship without rebuilding the APK; the APK only needs a rebuild for native plumbing such as Capacitor plugins, manifest changes, and FCM config.

## 8. Cross-refs

- `README.md` — phase status, server smoke test, WebSocket protocol details
- `INTENT.md` — why a v2 rewrite, what's deliberately out of scope
- `PLAN.md` — architecture, components, build order
- `TOOLING.md` — toolchain set for the dev pod, including agent auth bootstrap expectations
- `deploy/KOMODO.md` — one-time Komodo stack creation steps
- `deploy/docker-compose.yml`, `deploy/entrypoint.sh` — runtime orchestration

## 9. Status / backlog

Tracked inline in `README.md` and `uplift.md`. Move to Forgejo issues if it gets unwieldy.
