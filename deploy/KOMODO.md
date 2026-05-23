# Deploying MyDevEnv2 via Komodo

This repo's CI pipeline expects a Komodo stack named **`prod-mydevenv2`** with
its desired state in **`ops/personal/mydevenv2/`**. One-time setup:

## 1. Add the stack to the ops repo

```bash
cd ~/Working/Active/apps/ops
mkdir -p personal/mydevenv2
cp ~/Working/Active/apps/MyDevEnv2/deploy/docker-compose.yml personal/mydevenv2/
git add personal/mydevenv2 && git commit -m "add prod-mydevenv2 stack" && git push
```

## 2. Mint a server bearer token + Tailscale auth key

```bash
# Server token
openssl rand -hex 32
# → store in Infisical as MYDEVENV2_TOKEN (apps project, env prod)

# Tailscale reusable preauth key for the pod, from
# https://login.tailscale.com/admin/settings/keys
# → store as HOMELAB_MYDEVENV2_TAILSCALE_AUTH_KEY (apps project, env prod)
```

Then add both to the Komodo stack's `environment` field — Komodo does not
read from Infisical directly; it writes whatever you put in `environment`
into a `.env` file at deploy time.

## 3. Create the stack via the Komodo API

```bash
KOMODO_API_KEY=$(infisical secrets get HOMELAB_KOMODO_API_KEY \
  --domain https://se.sprooty.com \
  --projectId 76b1ebe1-3656-4cef-952c-30d5d489c6e7 --env prod --plain)
KOMODO_API_SECRET=$(infisical secrets get HOMELAB_KOMODO_API_SECRET \
  --domain https://se.sprooty.com \
  --projectId 76b1ebe1-3656-4cef-952c-30d5d489c6e7 --env prod --plain)
MYDEVENV2_TOKEN=$(infisical secrets get MYDEVENV2_TOKEN \
  --domain https://se.sprooty.com \
  --projectId 76b1ebe1-3656-4cef-952c-30d5d489c6e7 --env prod --plain)
TS_KEY=$(infisical secrets get HOMELAB_MYDEVENV2_TAILSCALE_AUTH_KEY \
  --domain https://se.sprooty.com \
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
      "file_paths": ["docker-compose.yml"],
      "environment": "MYDEVENV2_TOKEN=$MYDEVENV2_TOKEN\nHOMELAB_MYDEVENV2_TAILSCALE_AUTH_KEY=$TS_KEY\n"
    }
  }
}
EOF
```

(Server ID lookup: `POST /read` body `{"type":"ListServers"}` — match the
Node B periphery.)

## 4. First deploy

```bash
# Manual one-time deploy. After this, CI handles redeploys on every push to main.
curl -sS -X POST http://100.92.54.45:3011/execute \
  -H "X-Api-Key: $KOMODO_API_KEY" \
  -H "X-Api-Secret: $KOMODO_API_SECRET" \
  -H 'Content-Type: application/json' \
  -d '{"type":"DeployStack","params":{"stack":"prod-mydevenv2"}}'
```

## 5. Caddy entry (optional, recommended)

To reach the pod via `mydevenv2.sprooty.com` (Tailscale-served), add to
Node B's Caddyfile:

```caddyfile
mydevenv2.sprooty.com {
    reverse_proxy localhost:8910
}
```

Otherwise just open the Tailscale IP directly: `http://mydevenv2:8910` if
MagicDNS is on, or `http://100.x.y.z:8910`.

## 6. Workspace bind-mount

The compose mounts `/data/docker/mydevenv2/workspace` → `/home/sprooty/Working`
inside the container. To dogfood the real workspace, point that path at a
shared mount of the dev machine's `~/Working`. Options:

- **NFS** export `~/Working` from Sprooty-PC-UBNT, mount on Node B at
  `/data/docker/mydevenv2/workspace` (existing TrueNAS pattern in
  `~/truenas/`).
- **Syncthing** between Sprooty-PC-UBNT and Node B for offline-friendly sync.
- **Direct on Node B** — treat the pod as the canonical workspace and pull
  repos there.

Whichever you pick, the path inside the container stays `/home/sprooty/Working`
so all tooling and paths line up with the dev machine.
