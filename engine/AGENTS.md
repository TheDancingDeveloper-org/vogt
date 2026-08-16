# Agent Guide — the `engine/` subtree

This file is the canonical guidance for AI agents working on Vogt's session
engine. It was MyDevEnv2, merged into Vogt, and it now lives in this
repository's `engine/` subtree with `web/` and `mobile/` beside it at the
repository root. **Paths here are relative to the repository root, not to
`engine/`**, because a document that moved and kept its old relative paths is a
document that lies quietly.

**`docs/engine/` no longer exists** *(2026-08-15)*. The engine's eight
documents were consolidated into [`docs/ENGINE.md`](../docs/ENGINE.md), with
the runtime image in `docs/DEPLOYMENT.md` §10 and the stacks in §11. If a path
below ever names `docs/engine/`, it is stale and this table is the map:
`API_CONTRACT` → `ENGINE.md` §5, `ASSISTANT` → §6, `AGENT_TASKS` → §7,
`INTENT`/`PLAN` → §1–§2, `TOOLING` → `DEPLOYMENT.md` §10, `KOMODO` → §11,
`uplift` → `REQUIREMENTS.md` §7, `USER_GUIDE` → `docs/USER_GUIDE.md`.

Above this file sits [`AGENTS.md`](../AGENTS.md) at the repository root, which
owns the Python core and the repo-wide rules, and
`/home/sprooty/Working/AGENTS.md` — not linked, because it is a path on one
machine and a link that resolves nowhere else is worse than a name — which owns
the estate. There is no `CLAUDE.md` anywhere in this repository — the merge did
not carry MyDevEnv2's pointer file across, and the estate's `~/Working/CLAUDE.md`
exists only to redirect to `AGENTS.md`.

Documentation ownership for the engine:

- `docs/ENGINE.md` — what the engine is, running it, the wire contract, the
  assistant, agent tasks, the WebSocket protocol, smoke tests
- `engine/AGENTS.md` — coding-agent workflow, guardrails, project-specific rules
- `engine/README.md` — a pointer to the two above, and the two facts that bite
  before anybody has read either
- `docs/DEPLOYMENT.md` §10 — runtime/dev-pod toolchain inventory
- `docs/DEPLOYMENT.md` §11 — the standalone stacks: environment, rollout, recovery
- `docs/REQUIREMENTS.md` §7 — what was designed here and never built. **The
  engine has no separate backlog**; an outstanding item needs a requirement
  before it is anybody's plan.

Before changing files here:

- Read `/home/sprooty/Working/AGENTS.md` for workspace-wide infrastructure, CI/CD, secrets, commit rules, and service access paths.
- Read the repository-root `AGENTS.md` before touching anything outside `engine/`, `web/`, or `mobile/`.
- Follow this file for MyDevEnv2-specific build, deploy, and runtime guidance.
- Do not add a `CLAUDE.md` here; `AGENTS.md` is the only agent-guidance file this repository keeps, at both levels.

## 1. Quick reference

| | |
|---|---|
| Type | Rust (Axum) + Solid/Vite PWA (embedded via `rust-embed`) + Capacitor Android wrap |
| Repo | `github.com/TheDancingDeveloper-org/vogt`, subtree `engine/`. `repo.indexarr.net/indexarr/MyDevEnv2` is the pre-merge origin and remains the archive for the deprecated GPUI desktop client. |
| Cargo workspace root | `engine/` (`engine/Cargo.toml`, members `server` and `contract`). The repository root is a Python project, not a Rust workspace. |
| CI pipeline | `.github/workflows/ci.yml` (fmt/clippy/test, PWA typecheck and tests, Android shell) and `.github/workflows/build.yml` (the merged image, cosign-signed, to GHCR). The engine's Woodpecker pipeline is **not** in this tree: the fork vendored a copy at `engine/.woodpecker/server.yml`, it never ran here, and it has been deleted. `indexarr/MyDevEnv2` on the forge keeps the running copy, and that is the one the standalone stacks below are built by. |
| Deploys to | Komodo stack `prod-mydevenv2` (ops repo path `personal/mydevenv2/`) — target periphery is the one running mydevenv2.sprooty.com (see ops repo) |
| Image | `repo.indexarr.net/indexarr/mydevenv2` (`:latest` + `:<sha>`) |
| Runtime port(s) | `8910/tcp` (HTTP API + WebSocket attach + SSE; PWA served from same port) |
| Public URL | `https://mydevenv2.sprooty.com` |
| DB / state | None (Postgres-less by design). Sessions in-memory; push subscriptions persisted as JSON under `state_dir`. |
| Secrets used at runtime | `MYDEVENV2_TOKEN` (primary API bearer), optional `MYDEVENV2_EXTRA_TOKENS_JSON` (scoped JSON token list), `HOMELAB_MYDEVENV2_TAILSCALE_AUTH_KEY` (Tailscale userspace), `HOMELAB_MYDEVENV2_INFISICAL_CLIENT_ID` / `HOMELAB_MYDEVENV2_INFISICAL_CLIENT_SECRET` (required production agent service auth), optional `MYDEVENV2_FCM_SERVICE_ACCOUNT_JSON` for native FCM, optional `CONTEXTKEEPER_URL` / `CONTEXTKEEPER_API_TOKEN` for continuity — all in Infisical `apps` and pasted into the Komodo stack `environment`. VAPID keys are generated and persisted under `state_dir`. |

From-scratch rewrite of MyDevEnv v1 (a separate estate checkout, not part of this repository): same centrally-hosted, Tailscale-accessible dev environment goal, built without the v1 surface area of a code-server fork and multiple half-finished native clients. Active status: everything the merged product depends on is built and deployed; the legacy GPUI desktop client was deprecated on July 7, 2026 and left behind in the MyDevEnv2 repo by the merge. The old plan's "Phase 7", a KVM-backed Android emulator VM, was **never built and is not owed** — it is a withdrawal recorded in `docs/REQUIREMENTS.md` §7.3, not pending work. Reviving it would be a new requirement.

A pre-prod validation stack, `dev-mydevenv2`, also exists (`https://mydevenv2-dev.sprooty.com`, Komodo stack, desired state in ops's `personal/mydevenv2-dev/` on `main`) — see `docs/DEPLOYMENT.md` §11 for its deploy flow, disk layout, and why it exists.

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

See `docs/ENGINE.md` §1-§2 for what the engine owns and how it is shaped, and `docs/MERGE_MYDEVENV2.md` for why it is a separate process at all.

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

Optional TOML config at `mydevenv2.toml` (CLI > env > config); see
`docs/ENGINE.md` §3 for keys.

## 4. Deployment

Two pipelines exist and only one of them is in this repository, which is the
first thing to get straight before reading anything below. **This** tree is
built by GitHub Actions, described in `AGENTS.md` and in the workflow files
themselves. The Woodpecker pipeline that still builds `prod-mydevenv2` and
`dev-mydevenv2` belongs to `indexarr/MyDevEnv2` on the forge and is edited
there. The fork left a copy of it at `engine/.woodpecker/server.yml`; it never
ran here — Woodpecker's repository list does not contain Vogt, and Vogt is on
GitHub — and it has been deleted, along with its `install-sccache.sh`, because
a file describing how to publish a competing image and redeploy production is
worse than no file when nothing executes it.

Non-standard bits of the forge's pipeline, kept here because the stacks it
deploys are still running and the reasons are not written down there:

- Custom `clone:` block uses `git_auth_token` because the default OAuth clone started 403'ing at pipeline #17. Reference pattern in root Woodpecker pitfalls #2 / #6.
- `sccache` uses Redis on Node B (`100.92.54.45:6380`) for its `fmt` / `clippy` / `test` steps. Do not `apt install sccache`; Debian's package lacks Redis support. Its `install-sccache.sh` resolves the latest GitHub release at CI run time and verifies it against the sha256 digest that release's own API response publishes — not a hardcoded version/checksum. **This repository does not use it**: `ci.yml` caches with `Swatinem/rust-cache` instead, because that Redis is a Node B service that has OOM-crash-looped and a build cache able to take CI down is the wrong trade when the runners are what is scarce.
- `mkdir -p web/dist && touch web/dist/.placeholder` before `clippy` and `test` so `rust-embed` compiles before the image build produces the real bundle. That is the repository-root `web/dist/`, which is what `engine/server/src/assets.rs` embeds (`#[folder = "../../web/dist/"]`). `ci.yml` builds the real bundle first and keeps the placeholder only as a guard.
- `mobile-apk` builds the Capacitor signed release APK and uploads it to Forgejo releases API tag `apk-latest`, not the generic-package registry. Signing material comes from Woodpecker secrets `mydevenv2_android_keystore_base64`, `mydevenv2_android_keystore_password`, `mydevenv2_android_key_alias`, and `mydevenv2_android_key_password`. Idempotence is delete-then-create using `scripts/forgejo-api.sh` from `indexarr/ops`. That keystore and that registry stayed with the forge; `release.yml` signs from GitHub secrets and publishes the APK as a workflow artefact.
- `cimg/android` runs as `circleci` (UID 3434) but the workspace was cloned by `alpine/git` as root. That pipeline does `sudo chown -R circleci:circleci .` before pnpm.
- Its Komodo deploy pins the SHA in `ops/personal/mydevenv2/docker-compose.yml` through standard `scripts/komodo-deploy.sh` (`STACK_NAME=prod-mydevenv2 STACK_DIR=personal/mydevenv2`). Nothing in this repository does that or should: publishing is not deploying (NFR-D10), and Vogt has never deployed from CI.
- The native desktop client is deprecated as of July 7, 2026 and its `client/`
  tree did not come across in the merge — it stayed in the MyDevEnv2 repo,
  which is now its archive. Its old Linux/Windows release workflows were
  removed. A thin desktop wrapper, if ever reintroduced, starts from that
  archive, not from this tree.

Runtime container: multi-stage `engine/Dockerfile` produces an Ubuntu 26.04 runtime carrying the full `docs/DEPLOYMENT.md` §10 toolchain set, Sway, Selkies-GStreamer, Tailscale userspace, neutral service CLIs (`infisical`, `gh`, `git`, `curl`), and the embedded PWA. `engine/deploy/entrypoint.sh` orchestrates Tailscale -> optional Sway -> server. `docs/DEPLOYMENT.md` §10 owns the package/tool inventory; §11 owns the production compose and overlay details.

Codex and Claude are deliberately not installed by the production container bootstrap. The dev image installs both; its system `codex` launcher deliberately disables Codex's nested approvals and sandbox so trusted agents have normal container-user filesystem and network access across the complete `/home/sprooty/Working` workspace. The `opencode` CLI is bundled in every image; other AI clients remain user-managed in production. Production default-shell sessions are authenticated through `mydevenv2-agent-auth`; explicit-command sessions can invoke the helper directly.

Outstanding before the GUI tab is operational in production: set `START_SWAY=1`, set `GUI_STREAM_URL`, and verify Selkies from inside the pod. The stack itself, auth identity, workspace mount, and Komodo deploy path already exist.

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

The helper fetches Forgejo, Woodpecker, GitHub, Komodo, and Cadastre credentials into the child-process environment only. Do not persist fetched tokens in the workspace, shell profiles, local Git config, or logs. Cadastre uses the `HOMELAB_CADASTRE_HTTP_TOKEN` secret and the private endpoint `https://winrarhost.tailc7d3c.ts.net:18092/mcp`.

Use `mydevenv2-agent-auth check` to prove authenticated access to:

- Forgejo API: `https://repo.indexarr.net/api/v1/user`
- Forgejo git: `https://repo.indexarr.net/indexarr/ops.git`
- Woodpecker API: `https://ci.indexarr.net/api/user`
- GitHub org API: `gh api orgs/AusAgentSmith-org`
- Komodo API: `http://100.92.54.45:3011/read`
- Cadastre MCP: `https://winrarhost.tailc7d3c.ts.net:18092/mcp`

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
- Production compose requires the Infisical identity credentials and sets
  authenticated shells as the default. Direct host Docker access, including
  socket GID `984`, lives in the `engine/deploy/docker-compose.docker-socket.yml` overlay.
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

- API auth is bearer-token based, but tokens are no longer implicitly equivalent: the primary `MYDEVENV2_TOKEN` has full access, while optional entries from `MYDEVENV2_EXTRA_TOKENS_JSON` can be scoped to `sessions`, `filesystem-write`, `git-write`, `gui-control`, `agent-tasks-write`, `push-write`, `history-write`, and `assistant`. Public routes remain `/healthz`, `/readyz`, `/api/push/public-key`, and `/api/config`. Do not add new public routes without thinking about CSRF; the PWA stores the token in `localStorage` and sends it via `Authorization:` header. WebSocket currently falls back to a `?token=` query param.
- WebSocket attach protocol is ordered: `snapshot-start` text frame -> <=64 KiB binary scrollback chunks -> `snapshot-done` text -> live binary. Client text frames must be JSON (`{"type":"resize"|"ping"|...}`); binary frames are written verbatim to PTY stdin. Lag causes the server to send `{"type":"lag",...}` and close; the client should reattach.
- `workspace_root` is the boundary for all file APIs. Path-traversal is strict-component-checked, binary detection runs, and reads cap at 5 MiB. Do not introduce a file endpoint that bypasses these checks.
- Activity state machine drives push delivery: `idle` / `running` / `waiting-for-input` / `errored`. Push fires on entry to `waiting-for-input`. The heuristic is regex on stripped output tail; adjust with care because false positives become phone notifications.
- PWA bundle is baked at compile time. A `cargo build` without a fresh `pnpm build` ships stale frontend. The `web/dist/.placeholder` trick is for CI lint stages only; production builds must produce a real bundle.
- Push subscriptions auto-prune on 404/410 from the push endpoint. Do not add manual retry/cleanup on top; the prune is the cleanup.
- ContextKeeper continuity is **optional and must stay optional**. With `CONTEXTKEEPER_URL` or `CONTEXTKEEPER_API_TOKEN` unset, the routes under `/api/contextkeeper/*` answer `404`, `SessionSummary.continuity` is omitted, and every terminal reads as unprotected — which is also what an outage looks like, deliberately. Nothing in the terminal lifecycle may come to depend on the sidecar being up: roster enrichment reads a cache refreshed in the background precisely so a slow sidecar cannot become MyDevEnv2's latency.
- The ContextKeeper token is server-side only. The browser calls same-origin `/api/contextkeeper/*` and the server proxies; never forward the token to the client or let the PWA call the sidecar directly. The proxy is an allow-list of the operations the UI needs, because ContextKeeper also exposes prune and maintenance routes.
- Render ContextKeeper's continuation recipe; do not reinvent provider policy in the UI. It picks the rung (reattach / resume / fork / bundle) and supplies the command, cwd, and env; MyDevEnv2 creates the PTY from them verbatim, because the correlation identifiers that bind the new session to its work travel in that env. A bundle recovery is never launched without showing the bundle first — approval is a human decision about one specific bundle.
- The sidecar is reached through the pinned `extra_hosts` entry, not container DNS: this container runs Tailscale, which overwrites `/etc/resolv.conf` and breaks Docker service-name resolution.
- Mobile WebView loads whatever front door the APK was built against — `VOGT_ANDROID_SERVER_URL` at `cap sync` time, which has no default on purpose (FR-M1; the reasoning is in `mobile/capacitor.config.ts`). UI changes ship without rebuilding the APK; the APK only needs a rebuild for native plumbing such as Capacitor plugins, manifest changes, and FCM config — or to point it at a different front door.

## 8. Cross-refs

- `docs/ENGINE.md` — everything about the engine: what it owns, running it, the
  wire contract (§5), the assistant (§6), agent tasks (§7), the WebSocket
  protocol and the smoke tests
- `docs/DEPLOYMENT.md` §10 — toolchain set for the dev pod, including agent-auth
  bootstrap expectations
- `docs/DEPLOYMENT.md` §11 — one-time Komodo stack creation, overlays, recovery
- `docs/MERGE_MYDEVENV2.md` — why the engine is a separate process in a separate
  language, argued rather than assumed
- `engine/deploy/docker-compose.yml`, `engine/deploy/entrypoint.sh` — runtime orchestration
- `AGENTS.md` (repository root) — the Python core, the operation registry, and the repo-wide rules

## 9. Status / backlog

The engine has no backlog of its own. What was designed here and never built is
`docs/REQUIREMENTS.md` §7, where each gap is either a numbered requirement or a
recorded withdrawal. Vogt's own roadmap is
`docs/ROADMAP.md` and is a separate list; do not merge the two. Move backlog
items to Forgejo issues if they become unwieldy.
