> **Workspace root:** [`~/Working/CLAUDE.md`](/home/sprooty/Working/CLAUDE.md) — infrastructure, CI/CD, shared crates, secrets, commit rules.

# MyDevEnv2

Centrally-hosted, Tailscale-accessible dev environment driven from any browser, mobile PWA, or Capacitor APK. Single Axum binary owning PTY sessions, file APIs, git APIs, push delivery, and an embedded Solid+Vite PWA. **Active — Phase 1–6 code-complete; Phase 7 (KVM-backed Android emulator VM) pending.**

From-scratch rewrite of `../MyDevEnv` — same goal, built without the v1 surface area (code-server fork, multiple half-finished native clients).

## 1. Quick reference

| | |
|---|---|
| Type | Rust (Axum) + Solid/Vite PWA (embedded via `rust-embed`) + Capacitor Android wrap |
| Repo | `repo.indexarr.net/indexarr/MyDevEnv2` |
| CI pipeline | `.woodpecker.yml` — fmt/clippy/test → web-typecheck → mobile-apk → buildx → komodo-deploy |
| Deploys to | Komodo stack `prod-mydevenv2` (ops repo path `personal/mydevenv2/`) — target periphery is the one running mydevenv2.sprooty.com (see ops repo) |
| Image | `repo.indexarr.net/indexarr/mydevenv2` (`:latest` + `:<sha>`) |
| Runtime port(s) | `8910/tcp` (HTTP API + WebSocket attach + SSE; PWA served from same port) |
| Public URL | `https://mydevenv2.sprooty.com` |
| DB / state | None (Postgres-less by design). Sessions in-memory; push subscriptions persisted as JSON under `state_dir`. |
| Secrets used at runtime | `MYDEVENV2_TOKEN` (bearer for API), `HOMELAB_MYDEVENV2_TAILSCALE_AUTH_KEY` (Tailscale userspace), `HOMELAB_MYDEVENV2_VAPID_*` + FCM service account JSON for push — all in Infisical `apps` and pasted into the Komodo stack `environment` |

## 2. Architecture

Single binary, multi-tab PWA. `rust-embed` bakes the Solid PWA bundle into the Rust release at compile time.

```
Browser / iOS PWA / Android Capacitor APK
   │   HTTPS + WSS via Caddy at mydevenv2.sprooty.com
   ▼
Axum server (mydevenv2-server) — bind :8910
   ├── /api/sessions/*        PTY lifecycle  (bearer-token)
   ├── /api/sessions/:id/attach   WebSocket (snapshot replay → live binary frames)
   ├── /api/events            SSE — server-wide session state changes
   ├── /api/dir, /api/tree, /api/files, /api/search   workspace_root-scoped, ripgrep
   ├── /api/git/{status,diff,log,branch}              shells out to git
   ├── /api/push/*            VAPID web-push + FCM HTTP v1
   ├── /api/gui/*             swaymsg-driven GUI launcher (Phase 5)
   └── /                      embedded PWA (rust-embed)
```

See `PLAN.md` and `INTENT.md` for design decisions and rationale.

## 3. Build / test / run

```bash
# Run server (mint a token first, ≥16 chars)
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
# terminal 2: cd web && pnpm dev   → http://127.0.0.1:5173
```

Optional TOML config at `mydevenv2.toml` (CLI > env > config); see README for keys.

## 4. Deployment

**Pipeline non-standard bits** — most steps follow the root §CI/CD pattern, but these are project-specific and worth knowing:

- **Custom `clone:` block** using `git_auth_token` because the default OAuth clone started 403'ing at pipeline #17. Reference pattern in root §Woodpecker pitfalls #2 / #6.
- **sccache → Redis on Node B (`100.92.54.45:6380`)** for `fmt` / `clippy` / `test` steps. Don't `apt install sccache` — Debian's package lacks Redis support; the pipeline fetches the GitHub release binary v0.10.0 (see [`feedback_sccache_apt.md`](/home/sprooty/.claude/projects/-home-sprooty-Working/memory/feedback_sccache_apt.md)).
- **`mkdir -p web/dist && touch web/dist/.placeholder`** in `clippy` and `test` steps so `rust-embed` compiles before `build-and-push` produces the real bundle.
- **`mobile-apk` step** builds the Capacitor debug APK and uploads it to the Forgejo releases API (tag `apk-latest`), not the generic-package registry. Idempotence via delete-then-create using `scripts/forgejo-api.sh` from `indexarr/ops`. Sideload:
  ```bash
  curl -fsSL -o app.apk -H "Authorization: token $FORGEJO_TOKEN" \
    https://repo.indexarr.net/api/v1/repos/indexarr/MyDevEnv2/releases/tags/apk-latest \
    # → parse assets[].browser_download_url
  ```
- **`cimg/android` UID quirk**: image runs as `circleci` (UID 3434) but the workspace was cloned by `alpine/git` as root. Pipeline does `sudo chown -R circleci:circleci .` before pnpm.
- **Komodo deploy** pins the SHA in `ops/personal/mydevenv2/docker-compose.yml` via standard `scripts/komodo-deploy.sh` (`STACK_NAME=prod-mydevenv2 STACK_DIR=personal/mydevenv2`).

**Runtime container**: multi-stage `Dockerfile` produces an Ubuntu 26.04 runtime carrying the full `TOOLING.md` toolchain set, Sway, Selkies-GStreamer (for the GUI tab), Tailscale userspace, and the embedded PWA. `deploy/entrypoint.sh` orchestrates Tailscale → optional Sway → server.

**Outstanding before Phase 5 GUI tab is verifiable**: items 1–5 in `README.md` § "What's still on the user's plate".

## 5. Rules for AI agents

- **Bearer token gates everything except `/healthz`, `/api/push/public-key`, `/api/config`.** Don't add new public routes without thinking about CSRF — the PWA stores the token in `localStorage` and sends it via `Authorization:` header (WebSocket falls back to a `?token=` query param).
- **WebSocket attach protocol is ordered**: `snapshot-start` text frame → ≤64 KiB binary scrollback chunks → `snapshot-done` text → live binary. Client text frames must be JSON (`{"type":"resize"|"ping"|...}`); binary frames are written verbatim to PTY stdin. Lag → server sends `{"type":"lag",...}` and closes — client should reattach.
- **`workspace_root` is the boundary for all file APIs.** Path-traversal is strict-component-checked, binary detection runs, and reads cap at 5 MiB. Don't introduce a file endpoint that bypasses these checks.
- **Activity state machine drives push delivery**: `idle` / `running` / `waiting-for-input` / `errored`. Push fires on entry to `waiting-for-input`. Heuristic is regex on stripped output tail — adjust with care, false positives = phone notifications.
- **PWA bundle is baked at compile time.** A `cargo build` without a fresh `pnpm build` ships stale frontend. The `web/dist/.placeholder` trick is for CI lint stages only — production builds must produce a real bundle.
- **Push subscriptions auto-prune on 404/410** from the push endpoint. Don't add manual retry/cleanup on top — the prune is the cleanup.
- **Mobile WebView loads `https://mydevenv2.sprooty.com` directly** — UI changes ship without rebuilding the APK. The APK only needs a rebuild for native plumbing (Capacitor plugins, manifest changes, FCM config).

## 6. Cross-refs

- `README.md` — phase status, server smoke test, WebSocket protocol details
- `INTENT.md` — why a v2 rewrite, what's deliberately out of scope
- `PLAN.md` — architecture, components, build order
- `TOOLING.md` — toolchain set for the dev pod (derived from v1 Dockerfile)
- `deploy/KOMODO.md` — one-time Komodo stack creation steps
- `deploy/docker-compose.yml`, `deploy/entrypoint.sh` — runtime orchestration

## 7. Status / backlog

Tracked inline in `README.md` § Status. Move to Forgejo issues if it gets unwieldy.
