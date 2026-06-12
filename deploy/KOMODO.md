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

## 2. Mint runtime secrets and the optional agent identity

```bash
# Server token
openssl rand -hex 32
# -> store in Infisical as MYDEVENV2_TOKEN (apps project, env prod)

# Tailscale reusable preauth key for the pod, from
# https://login.tailscale.com/admin/settings/keys
# -> store as HOMELAB_MYDEVENV2_TAILSCALE_AUTH_KEY (apps project, env prod)
```

For authenticated agent access, create an Infisical Machine Identity:

1. Open Organization Settings -> Machine Identities -> Create.
2. Name it `mydevenv2-agents`.
3. Enable Universal Auth and create client credentials.
4. Grant read-only access to the `cicd`, `infrastructure`, and `apps` projects.
5. Store the credentials in Infisical `apps` / `prod` as:
   - `HOMELAB_MYDEVENV2_INFISICAL_CLIENT_ID`
   - `HOMELAB_MYDEVENV2_INFISICAL_CLIENT_SECRET`

Paste all four runtime values into the Komodo stack `environment` field. Komodo
does not read Infisical directly; it writes its environment field into a `.env`
file at deploy time. The agent identity is optional for the MyDevEnv2 server, but
`mydevenv2-agent-auth` will remain unavailable until both credentials are set.

Codex and Claude are not installed or authenticated by this bootstrap. They are
optional clients and can use the neutral `mydevenv2-agent-auth` helper when
installed by a user.

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
      "file_paths": ["docker-compose.yml"],
      "environment": "MYDEVENV2_TOKEN=$MYDEVENV2_TOKEN\nHOMELAB_MYDEVENV2_TAILSCALE_AUTH_KEY=$TS_KEY\nHOMELAB_MYDEVENV2_INFISICAL_CLIENT_ID=$AGENT_CLIENT_ID\nHOMELAB_MYDEVENV2_INFISICAL_CLIENT_SECRET=$AGENT_CLIENT_SECRET\n"
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

The compose mounts `/mnt/2tnvme/docker/volumes/mydevenv2/workspace` → `/home/sprooty/Working`
inside the container. To dogfood the real workspace, point that path at a
shared mount of the dev machine's `~/Working`. Options:

- **NFS** export `~/Working` from Sprooty-PC-UBNT, mount on Node B at
  `/mnt/2tnvme/docker/volumes/mydevenv2/workspace` (existing TrueNAS pattern in
  `~/truenas/`).
- **Syncthing** between Sprooty-PC-UBNT and Node B for offline-friendly sync.
- **Direct on Node B** — treat the pod as the canonical workspace and pull
  repos there.

Whichever you pick, the path inside the container stays `/home/sprooty/Working`
so all tooling and paths line up with the dev machine.
