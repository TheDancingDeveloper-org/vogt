# Agent Guide — the `engine/` subtree

This file is the canonical guidance for AI agents working on Vogt's session
engine. It was a standalone product, merged into Vogt, and it now lives in this
repository's `engine/` subtree with `web/` and `mobile/` beside it at the
repository root. **Paths here are relative to the repository root, not to
`engine/`**, because a document that moved and kept its old relative paths is a
document that lies quietly.

The engine's documentation is [`docs/ENGINE.md`](../docs/ENGINE.md): what it
is, building and running it, the wire contract (§5), the assistant (§6), agent
tasks (§7), and its optional integrations (§9). Production deployment of the
merged image is [`docs/DEPLOYMENT.md`](../docs/DEPLOYMENT.md).

Above this file sits [`AGENTS.md`](../AGENTS.md) at the repository root, which
owns the Python core and the repo-wide rules. `CLAUDE.md` files in this
repository are pointers to the `AGENTS.md` beside them and carry no rules of
their own.

Documentation ownership for the engine:

- `docs/ENGINE.md` — what the engine is, running it, the wire contract, the
  assistant, agent tasks, the WebSocket protocol, smoke tests, optional
  integrations
- `engine/AGENTS.md` — coding-agent workflow, guardrails, project-specific rules
- `engine/README.md` — a pointer to the two above, and the two facts that bite
  before anybody has read either
- `docs/DEPLOYMENT.md` — deploying the images, including the optional engine
- **The engine has no separate backlog**; outstanding work is a GitHub issue
  or an entry in `docs/ROADMAP.md`.

Before changing files here:

- Read the repository-root `AGENTS.md` before touching anything outside `engine/`, `web/`, or `mobile/`.
- Follow this file for engine-specific build, deploy, and runtime guidance.
- Do not add rules to a `CLAUDE.md`; `AGENTS.md` is the guidance file at both levels.

## 1. Quick reference

| | |
|---|---|
| Type | Rust (Axum) + Solid/Vite PWA (embedded via `rust-embed`) + Capacitor Android wrap |
| Repo | `github.com/TheDancingDeveloper-org/vogt`, subtree `engine/` |
| Cargo workspace root | `engine/` (`engine/Cargo.toml`, members `server` and `contract`). The repository root is a Python project, not a Rust workspace. |
| CI pipeline | `.github/workflows/ci.yml` (fmt/clippy/test, PWA typecheck and tests, Android shell) and `.github/workflows/build.yml` (the merged image, cosign-signed, to GHCR). |
| Image | built from `engine/Dockerfile` with the repository root as context; `engine/Dockerfile.pod` is the toolchain base it starts from (`docs/ENGINE.md` §3) |
| Runtime port(s) | `8910/tcp` (HTTP API + WebSocket attach + SSE; PWA served from same port; `/api/vogt`, `/mcp` and `/ui-legacy` proxied to the core) |
| DB / state | No database. Sessions in-memory; agent tasks, push subscriptions and the assistant log persisted under `state_dir` (JSON and SQLite). |
| Secrets used at runtime | `MYDEVENV2_TOKEN` (primary API bearer — the CLI parser still reads this name only), optional `ENGINE_EXTRA_TOKENS_JSON` (scoped JSON token list), optional `ENGINE_FCM_SERVICE_ACCOUNT_JSON` for native FCM, optional `ENGINE_ASSISTANT_API_KEY` and the speech keys (`docs/ENGINE.md` §6), optional `VOGT_CORE_TOKEN` / `VOGT_CORE_TOKEN_FILE` for the front door. VAPID keys are generated and persisted under `state_dir`. |

A from-scratch rewrite of an earlier centrally-hosted dev environment: the
same goal, built without a code-server fork and multiple half-finished native
clients. Everything the merged product depends on is built; the native desktop
client was deprecated on July 7, 2026 and left behind in the pre-merge
repository. A KVM-backed Android emulator VM from the old plan was **never
built and is not owed**; reviving it would be a new requirement.

## 2. Architecture

Single binary, multi-tab PWA. `rust-embed` bakes the Solid PWA bundle into the Rust release at compile time.

```text
Browser / iOS PWA / Android Capacitor APK
   |   HTTPS + WSS via your reverse proxy
   v
Axum server (mydevenv2-server) — bind :8910
   |-- /api/sessions/*             PTY lifecycle (bearer-token)
   |-- /api/sessions/:id/attach    WebSocket (snapshot replay -> live binary frames)
   |-- /api/events                 SSE — server-wide session state changes
   |-- /api/dir, /api/tree, /api/files, /api/search   workspace_root-scoped, ripgrep
   |-- /api/git/{status,diff,log,branch}              shells out to git
   |-- /api/push/*                 VAPID web-push + FCM HTTP v1
   |-- /api/gui/*                  compositor-driven GUI launcher
   |-- /api/agent-tasks/*          scheduled agent runs
   |-- /api/assistant/*            the tool-use assistant (404 until configured)
   |-- /api/vogt/*, /mcp, /ui-legacy   proxied to vogt-core on loopback
   `-- /                            embedded PWA (rust-embed)
```

See `docs/ENGINE.md` §1-§2 for what the engine owns and how it is shaped, and `docs/DESIGN.md` for why it is a separate process at all.

## 3. Build / test / run

Every `cargo` command runs from `engine/`; every `pnpm` command runs from `web/`
(or `mobile/`). Run them from the repository root and cargo will tell you there
is no manifest there, which is correct — the root is the Python project.

Each line below starts from the repository root; the subshells are so they all
still do.

```bash
# Run server (mint a token first, >=16 chars)
export MYDEVENV2_TOKEN="$(openssl rand -hex 24)"
(cd engine && cargo run -p mydevenv2-server -- --bind 127.0.0.1:8910)

# Tests
(cd engine && cargo test --all)    # unit + integration (HTTP + WS)
(cd web && pnpm typecheck)         # PWA TS check

# Refresh embedded PWA bundle (cargo bakes the repo-root web/dist/ at build time)
(cd web && pnpm install && pnpm build)
(cd engine && cargo build --release)

# UI development (Vite proxies /api + WS to the backend):
# terminal 1: cd engine && cargo run -p mydevenv2-server -- --bind 127.0.0.1:8910
# terminal 2: cd web && pnpm dev   -> http://127.0.0.1:5173
```

Optional TOML config via `--config` (CLI > env > config); see
`docs/ENGINE.md` §3 for keys. Engine settings are `ENGINE_*`; the legacy
`MYDEVENV2_*` names are still accepted as aliases, and the token/bind/config
CLI flags are read only under the legacy names.

## 4. Deployment

**Publishing is not deploying.** `.github/workflows/build.yml` builds the
merged image and pushes it to GHCR; nothing in this repository deploys it
anywhere (NFR-D10). How to run the published images, and how to build and run
the merged one, is `docs/DEPLOYMENT.md`; the engine-specific build story
(stages, build args, the pod base, the helper scripts) is `docs/ENGINE.md` §3.

Things about the build that are easy to get wrong:

- `rust-embed` compiles `web/dist/` into the binary, so `clippy` and `test`
  need the directory to exist. `ci.yml` builds the real bundle first and keeps
  `mkdir -p web/dist && touch web/dist/.placeholder` only as a guard. A
  production build must embed a real bundle.
- `engine/Dockerfile` takes the repository root as its build context and
  requires `CORE_IMAGE` (the published core image, by digest in CI) — it lifts
  the core out of that image rather than building it a second time.
- The runtime stage starts from `engine/Dockerfile.pod`, the dev-pod toolchain
  base, published separately and passed in by digest as `POD_BASE_IMAGE`.
  Toolchain changes go there; per-commit things (the agent CLIs, the optional
  Cadastre bridge) stay in `engine/Dockerfile`.
- `release.yml` signs the Android APK from GitHub secrets (a keystore the
  repository does not carry; the job is inert until one is configured) and
  publishes it as a workflow artefact.
- The native desktop client is deprecated as of July 7, 2026 and its
  `client/` tree did not come across in the merge. A thin desktop wrapper, if
  ever reintroduced, starts from that archive, not from this tree.

Runtime container: `engine/Dockerfile` produces an Ubuntu runtime carrying the
pod toolchain, an optional headless compositor and stream for the GUI tab,
neutral service CLIs (`gh`, `git`, `curl`), and the embedded PWA.
`engine/deploy/entrypoint.sh` orchestrates optional VPN join -> optional
compositor -> optional vogt-core on loopback -> the engine.

`codex` and `claude` are deliberately not installed by default
(`INSTALL_AI_CLIENTS=false`); a deployment either builds with the flag or
manages the CLIs in the pod's persisted home. The system `codex` launcher
installed with the flag disables Codex's nested approvals and sandbox, because
the pod is the isolation boundary. The `opencode` CLI is bundled in every
image.

The GUI tab is off until an operator sets `START_SWAY=1`, sets
`GUI_STREAM_URL`, verifies a launched process renders through the stream from
inside the pod, and only then sets `GUI_STREAM_VERIFIED=1`.

## 5. Agent service authentication

The image carries a credential broker, `mydevenv2-agent-auth`
(`engine/deploy/agent-auth.sh`):

```bash
mydevenv2-agent-auth check
mydevenv2-agent-auth run -- <command>
mydevenv2-agent-auth shell
```

With `ENGINE_AUTO_AGENT_AUTH=1`, sessions created without an explicit
`command` automatically use `mydevenv2-agent-auth shell`. Explicit-command
sessions are not wrapped; the two "(protected)" session templates wrap their
agent CLI explicitly.

The helper fetches service credentials (GitHub and others) from a secrets
manager into the child-process environment only. Do not persist fetched
tokens in the workspace, shell profiles, local Git config, or logs.

**As shipped, the script is written against the maintainer's own secrets
manager, project ids and service list** — it is an optional integration
(`docs/ENGINE.md` §9), off by default, and a public deployment should treat it
as a template for its own broker or leave it off. With it off, sessions are
plain shells and nothing in the engine misses it. The lesson its history
carries: a container created with empty broker credentials starts a
healthy-looking server whose sessions silently have no service access, so a
deployment that *requires* the broker should set
`MYDEVENV2_AGENT_AUTH_REQUIRED=1` (read by the entrypoint) and let startup
fail loudly instead.

## 6. Workspace

The engine serves one `workspace_root` (default `~/Working`; inside the image
that is the pod user's `~/Working`, declared as a volume). Every file, search and git route is
scoped to it, and `cwd` on a session is resolved inside it. The image does
not run an SSH server: the workspace is a bind mount or volume on the host,
and anything that synchronises it (rsync, mutagen, a shared filesystem) is a
deployment's own choice.

## 7. Rules for AI agents

- API auth is bearer-token based, but tokens are no longer implicitly equivalent: the primary token has full access, while optional entries from `ENGINE_EXTRA_TOKENS_JSON` can be scoped to `sessions`, `filesystem-write`, `git-write`, `gui-control`, `agent-tasks-write`, `push-write`, `history-write`, `assistant` and `vogt-write`. Public routes remain `/healthz`, `/readyz`, `/api/push/public-key`, and `/api/config`. Do not add new public routes without thinking about CSRF; the PWA stores the token in `localStorage` and sends it via `Authorization:` header. WebSocket currently falls back to a `?token=` query param.
- WebSocket attach protocol is ordered: `snapshot-start` text frame -> <=64 KiB binary scrollback chunks -> `snapshot-done` text -> live binary. Client text frames must be JSON (`{"type":"resize"|"ping"|...}`); binary frames are written verbatim to PTY stdin. Lag causes the server to send `{"type":"lag",...}` and close; the client should reattach.
- `workspace_root` is the boundary for all file APIs. Path-traversal is strict-component-checked, binary detection runs, and reads cap at 5 MiB. Do not introduce a file endpoint that bypasses these checks.
- Activity state machine drives push delivery: `idle` / `running` / `waiting-for-input` / `errored`. Push fires on entry to `waiting-for-input`. The heuristic is regex on stripped output tail; adjust with care because false positives become phone notifications.
- PWA bundle is baked at compile time. A `cargo build` without a fresh `pnpm build` ships stale frontend. The `web/dist/.placeholder` trick is for CI lint stages only; production builds must produce a real bundle.
- Push subscriptions auto-prune on 404/410 from the push endpoint. Do not add manual retry/cleanup on top; the prune is the cleanup.
- Mobile WebView loads whatever front door the APK was built against — `VOGT_ANDROID_SERVER_URL` at `cap sync` time, which has no default on purpose (FR-M1; the reasoning is in `mobile/capacitor.config.ts`). UI changes ship without rebuilding the APK; the APK only needs a rebuild for native plumbing such as Capacitor plugins, manifest changes, and FCM config — or to point it at a different front door.

## 8. Cross-refs

- `docs/ENGINE.md` — everything about the engine: what it owns, building and
  running it (§3), the wire contract (§5), the assistant (§6), agent tasks
  (§7), optional integrations (§9)
- `docs/DEPLOYMENT.md` — deploying the published images and the merged one
- `docs/DESIGN.md` — the product's architecture, including why the engine is a
  separate process in a separate language
- `engine/deploy/entrypoint.sh`, `engine/deploy/docker-compose.yml` — runtime
  orchestration and a sample standalone compose file (a template, not a
  deployment)
- `AGENTS.md` (repository root) — the Python core, the operation registry, and the repo-wide rules

## 9. Status / backlog

The engine has no backlog of its own. Outstanding engine work is a GitHub
issue on the repository; Vogt's roadmap is `docs/ROADMAP.md` and is a
separate list; do not merge the two.
