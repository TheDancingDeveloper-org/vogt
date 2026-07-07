# Deploying MyDevEnv2 via Komodo

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
