# Deploying MyDevEnv2 via Komodo

This file is the source of truth for production stack layout, required
environment, overlays, rollout, and recovery. Keep product status in
`README.md` and tool inventory in `TOOLING.md`.

Production currently runs as the Komodo stack **`prod-mydevenv2`**. Desired
state lives in the `indexarr/ops` repo at **`personal/mydevenv2/`**. The stack
serves the Rust/Axum API and embedded PWA from port `8910`, with Caddy in
front at `https://mydevenv2.sprooty.com`.

Normal deploy flow:

```text
push to main touching server/web/mobile/deploy paths
  -> .woodpecker/server.yml
  -> fmt / clippy / test / web-typecheck / mobile-apk
  -> Docker buildx pushes repo.indexarr.net/indexarr/mydevenv2:latest + :<sha>
  -> ops/personal/mydevenv2/docker-compose.yml is bumped to :<sha>
  -> Komodo DeployStack runs prod-mydevenv2
```

Client-only pushes under `client/**` intentionally do not rebuild or redeploy
the production server image. The native client is deprecated and no longer has
active Woodpecker workflows, so those legacy-only edits do not produce a
supported release artifact.

Manual redeploy of the currently pinned image is safe when you need Komodo to
restart/recreate the stack without a new server image. Use the shared
`ops/scripts/komodo-deploy.sh` with `IMAGE_TAG` set to the tag already pinned in
ops, or call Komodo `DeployStack` directly. Do not invent SSH/docker-compose
deploy steps.

The rest of this file is bootstrap/recovery reference for recreating the stack.

## 1. Add the stack to the ops repo

```bash
cd ~/Working/Active/apps/ops
mkdir -p personal/mydevenv2
cp ~/Working/Active/apps/MyDevEnv2/deploy/docker-compose.yml personal/mydevenv2/
cp ~/Working/Active/apps/MyDevEnv2/deploy/docker-compose.docker-socket.yml personal/mydevenv2/
git add personal/mydevenv2 && git commit -m "add prod-mydevenv2 stack" && git push
```

## 1a. Choose a Docker access mode

The base `docker-compose.yml` is intentionally socketless. Add one of these
overlay files to the Komodo stack `file_paths` list depending on the trust
boundary you want:

- `docker-compose.docker-socket.yml`
  Current personal-homelab mode. Mounts `/var/run/docker.sock` directly into
  the pod and adds the host socket group so `docker` works in normal sessions.
- No overlay
  Lower-privilege mode. Docker CLI remains installed in the image, but there is
  no daemon socket inside the pod, so Docker commands fail closed instead of
  silently inheriting host control.

Current production uses the direct socket overlay because authenticated agent
sessions and repo workflows still rely on host-daemon access.

## 2. Mint runtime secrets and the agent identity

```bash
# Server token
openssl rand -hex 32
# -> store in Infisical as MYDEVENV2_TOKEN (apps project, env prod)

# Optional: scoped API tokens as a JSON array. Capability names:
# sessions, filesystem-write, git-write, gui-control, agent-tasks-write,
# push-write, history-write
# -> store as MYDEVENV2_EXTRA_TOKENS_JSON if you want non-admin tokens

# Tailscale reusable preauth key for the pod, from
# https://login.tailscale.com/admin/settings/keys
# -> store as HOMELAB_MYDEVENV2_TAILSCALE_AUTH_KEY (apps project, env prod)
```

Create an Infisical Machine Identity for production agent access:

1. Open Organization Settings -> Machine Identities -> Create.
2. Name it `mydevenv2-agents`.
3. Enable Universal Auth and create client credentials.
4. Grant read-only access to the `cicd`, `infrastructure`, and `apps` projects.
5. Store the credentials in Infisical `apps` / `prod` as:
   - `HOMELAB_MYDEVENV2_INFISICAL_CLIENT_ID`
   - `HOMELAB_MYDEVENV2_INFISICAL_CLIENT_SECRET`

Paste the runtime values into the Komodo stack `environment` field. Komodo does
not read Infisical directly; it writes its environment field into a `.env` file
at deploy time. The production compose rejects empty identity values and the
entrypoint validates credential retrieval before starting the server.

Current production environment also expects:

- `MYDEVENV2_FCM_SERVICE_ACCOUNT_JSON` — single-line Firebase service-account
  JSON for FCM HTTP v1. Empty disables native FCM while web-push still works.
- `DOCKER_SOCKET_GID` — optional override for the host docker socket group
  added by `docker-compose.docker-socket.yml`. Node B currently uses `984`.

Codex and Claude are not installed by this bootstrap. They are optional clients;
default interactive sessions are authenticated through the neutral
`mydevenv2-agent-auth` helper when installed by a user.

## 3. Create the stack via the Komodo API

```bash
KOMODO_API_KEY=$(infisical secrets get HOMELAB_KOMODO_API_KEY \
  --domain http://100.92.54.45:8400 \
  --projectId 76b1ebe1-3656-4cef-952c-30d5d489c6e7 --env prod --plain)
KOMODO_API_SECRET=$(infisical secrets get HOMELAB_KOMODO_API_SECRET \
  --domain http://100.92.54.45:8400 \
  --projectId 76b1ebe1-3656-4cef-952c-30d5d489c6e7 --env prod --plain)
MYDEVENV2_TOKEN=$(infisical secrets get MYDEVENV2_TOKEN \
  --domain http://100.92.54.45:8400 \
  --projectId 76b1ebe1-3656-4cef-952c-30d5d489c6e7 --env prod --plain)
TS_KEY=$(infisical secrets get HOMELAB_MYDEVENV2_TAILSCALE_AUTH_KEY \
  --domain http://100.92.54.45:8400 \
  --projectId 76b1ebe1-3656-4cef-952c-30d5d489c6e7 --env prod --plain)
AGENT_CLIENT_ID=$(infisical secrets get HOMELAB_MYDEVENV2_INFISICAL_CLIENT_ID \
  --domain http://100.92.54.45:8400 \
  --projectId 76b1ebe1-3656-4cef-952c-30d5d489c6e7 --env prod --plain)
AGENT_CLIENT_SECRET=$(infisical secrets get HOMELAB_MYDEVENV2_INFISICAL_CLIENT_SECRET \
  --domain http://100.92.54.45:8400 \
  --projectId 76b1ebe1-3656-4cef-952c-30d5d489c6e7 --env prod --plain)

curl -sS -X POST http://100.92.54.45:3011/write \
  -H "X-Api-Key: $KOMODO_API_KEY" \
  -H "X-Api-Secret: $KOMODO_API_SECRET" \
  -H 'Content-Type: application/json' \
  -d @- <<EOF
{
  "type": "CreateStack",
  "params": {
    "name": "prod-mydevenv2",
    "config": {
      "server_id": "node-b",
      "git_provider": "repo.indexarr.net",
      "repo": "indexarr/ops",
      "branch": "main",
      "run_directory": "personal/mydevenv2",
      "file_paths": ["docker-compose.yml", "docker-compose.docker-socket.yml"],
      "environment": "MYDEVENV2_TOKEN=$MYDEVENV2_TOKEN\nHOMELAB_MYDEVENV2_TAILSCALE_AUTH_KEY=$TS_KEY\nHOMELAB_MYDEVENV2_INFISICAL_CLIENT_ID=$AGENT_CLIENT_ID\nHOMELAB_MYDEVENV2_INFISICAL_CLIENT_SECRET=$AGENT_CLIENT_SECRET\n"
    }
  }
}
EOF
```

(Server ID lookup: `POST /read` body `{"type":"ListServers"}` — match the
Node B periphery.)

If you provision scoped API tokens, append `MYDEVENV2_EXTRA_TOKENS_JSON=...`
and, if needed, `MYDEVENV2_MUTATING_REQUEST_LIMIT_PER_MINUTE=...` to the stack
environment as well.

## 4. First deploy

```bash
# Manual one-time deploy. After this, server CI handles redeploys on applicable
# pushes to main.
curl -sS -X POST http://100.92.54.45:3011/execute \
  -H "X-Api-Key: $KOMODO_API_KEY" \
  -H "X-Api-Secret: $KOMODO_API_SECRET" \
  -H 'Content-Type: application/json' \
  -d '{"type":"DeployStack","params":{"stack":"prod-mydevenv2"}}'
```

## 5. Caddy entry

To reach the pod via `mydevenv2.sprooty.com`, Node B's Caddy routes to the host
port:

```caddyfile
mydevenv2.sprooty.com {
    reverse_proxy localhost:8910
}
```

The public route can be protected by Caddy basic auth before traffic reaches
the app; direct tailnet liveness/readiness are available at
`http://100.92.54.45:8910/healthz` and `http://100.92.54.45:8910/readyz` from
inside the tailnet.

## 6. Workspace bind-mount

The compose mounts `/mnt/2tnvme/docker/volumes/mydevenv2/workspace` → `/home/sprooty/Working`
inside the container. This is the active workspace mount on Node B:

- Node B host path: `/mnt/2tnvme/docker/volumes/mydevenv2/workspace`
- Container path: `/home/sprooty/Working`
- Local workspace path: `/home/sprooty/Working`

Workspace synchronization is handled outside the app. The workspace-root
`mutagen.yml` should target Node B at
`sprooty@100.92.54.45:/mnt/2tnvme/docker/volumes/mydevenv2/workspace`; do not
target the retired MyDevEnv v1 endpoint.

## 7. Docker boundary notes

Direct Docker socket access is convenient but high-trust:

- Any shell or API flow that can drive Docker inside the pod can control the
  host daemon.
- Scoped non-admin API tokens reduce HTTP blast radius, but an interactive
  shell session in the direct-socket overlay still inherits host-daemon access.
- For personal homelab use that may be acceptable; for broader sharing, keep
  the base socketless stack or interpose a purpose-built socket proxy in front
  of the daemon.

Operational token policy for production browsers:

- Treat `MYDEVENV2_TOKEN` as the admin/recovery token, not the default browser
  token.
- Store scoped browser tokens in the PWA Settings modal instead; it persists
  them only in that device's `localStorage`.
- Prefer one interactive scoped token for day-to-day browser use and a separate
  read-only token for lower-trust clients.
- If a browser needs GUI launch/kill regularly, mint a dedicated scoped token
  with `gui-control` instead of reusing the primary token.

## 8. Dev stack (dev-mydevenv2)

Pre-prod validation stack, per `uplift.md` "Environment Strategy: Dev vs
Prod". Runs the same image family, tagged `:dev`/`:dev-<sha>` instead of
`:latest`/`:<sha>`. Desired state lives in `indexarr/ops` at
**`personal/mydevenv2-dev/`**, on ops's **`main` branch** — same branch as
prod's `personal/mydevenv2/`. This is *not* symmetric with MyDevEnv2's own
dev/main split: `scripts/komodo-deploy.sh` (the shared CI helper that bumps
the pinned image tag) clones ops's default branch unconditionally, with no
branch parameter, so any stack it manages has to live on `main` regardless
of which environment it represents. (An ops `dev` branch was tried first and
abandoned for exactly this reason — see git history if reviving it, e.g. to
add branch support to the shared script instead.) Komodo's own periphery
pull for this stack is unaffected either way — it reads the stack's own
`branch` config directly, independent of this script. Served at
`https://mydevenv2-dev.sprooty.com` (Caddy on Node B, `reverse_proxy
localhost:8911`; prod holds `8910`).

```text
push to MyDevEnv2 `dev` branch
  -> .woodpecker/server.yml (build-and-push-dev, komodo-deploy-dev)
  -> Docker buildx pushes repo.indexarr.net/indexarr/mydevenv2:dev + :dev-<sha>
  -> ops (main branch) personal/mydevenv2-dev/docker-compose.yml bumped to :dev-<sha>
  -> Komodo DeployStack runs dev-mydevenv2
```

**Disk layout differs from prod on purpose.** Prod's `docker-compose.yml`
bind-mounts `home` and `tailscale` under `/mnt/2tnvme/docker/volumes/`, and
does **not** bind-mount the container's `/tmp` at all — that gap is why
prod's `/tmp` silently accumulated ~184GB of stale build/test scratch
directories inside the writable layer on the root disk (`nvme0n1p2`),
contributing to a near-full-root-disk incident. The dev stack instead uses a
dedicated disk to validate a fix before prod adopts it:

| Path (inside container) | Host path | Disk |
|---|---|---|
| `/home/sprooty` (`$MYDEVENV_HOME_CONTAINER_PATH`) | `/mnt/sdg/mydevenv2-dev/home` | `sdg3`, ext4, ~457G (see root `AGENTS.md` "Node B Disk Layout") |
| `/var/lib/tailscale` | `/mnt/sdg/mydevenv2-dev/tailscale` | same |
| `/tmp` | `/mnt/sdg/mydevenv2-dev/tmp` | same |
| `/home/sprooty/Working` (workspace) | `/mnt/2tnvme/docker/volumes/mydevenv2/workspace` | **same disk/path as prod** — intentional, see below |

The workspace bind-mount is deliberately **not** moved to `sdg` in this pass:
it's the same live `~/Working` data prod uses (two independent PTY-session
servers against the same files is no different from two terminal windows),
and migrating a live, actively-edited dataset is a separate, higher-stakes
step from moving per-stack state and scratch space. If/when prod's own
`/tmp` and `home`/`tailscale` state move to a dedicated disk (validated here
first), update this table and the row in root `AGENTS.md`'s disk layout
accordingly — and note whether prod moves to `sdg` alongside dev or gets its
own disk, since `sdg` was sized for dev-only validation, not necessarily to
hold both stacks' full state long-term.

### Nested Codex sandbox

Codex's Linux workspace sandbox runs commands through Bubblewrap and therefore
needs to create an unprivileged user namespace. The host enables unprivileged
user namespaces, but Docker's default seccomp and AppArmor profiles deny the
nested `unshare` operation: both `unshare -Ur true` and `bwrap` fail with
`Operation not permitted`. Codex then has to request an out-of-sandbox approval
for routine workspace commands.

The dev stack sets `security_opt` to `seccomp=unconfined` and
`apparmor=unconfined` so the inner Codex sandbox can enforce its own workspace
boundary. It does not use Docker privileged mode and does not add `SYS_ADMIN`.
Validate after every recreation from a **new** terminal session inside the dev
container:

```bash
unshare -Ur true
bwrap --ro-bind / / true
```

Both commands must exit zero. Then start a new Codex chat in workspace mode and
confirm a harmless command such as `pwd` runs without an escalation prompt.
Existing Codex chats retain the permission policy they were started with, so
they are not a valid post-deploy test. Promote these settings to prod only after
the dev validation succeeds; relaxing the outer container profiles increases
the importance of retaining Codex's inner sandbox for untrusted commands.
